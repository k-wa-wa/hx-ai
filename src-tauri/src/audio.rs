//! オーディオ入力 (HX Stomp など) から録音する。OS に触る部分だけを置き、
//! 録音の加工 (無音を削る・16kHz に落とすなど) は `sound` で行う。

use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use serde::Serialize;

use crate::sound::{self, MAX_RECORD_SECONDS, Raw};

#[derive(Debug, Clone, Serialize)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<InputDevice>, String> {
    let host = cpal::default_host();
    let default_id = host
        .default_input_device()
        .and_then(|d| d.id().ok())
        .map(|id| id.to_string());
    let devices = host
        .input_devices()
        .map_err(|e| format!("オーディオ入力デバイスを列挙できません: {e}"))?;
    let mut out = Vec::new();
    for d in devices {
        let Ok(id) = d.id() else { continue };
        let id = id.to_string();
        out.push(InputDevice {
            is_default: default_id.as_deref() == Some(id.as_str()),
            name: d.to_string(),
            id,
        });
    }
    Ok(out)
}

fn find_device(device_id: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    match device_id {
        Some(id) => {
            let parsed = id
                .parse()
                .map_err(|e| format!("デバイス ID {id} を読めません: {e}"))?;
            host.device_by_id(&parsed)
                .ok_or_else(|| format!("オーディオ入力デバイスが見つかりません: {id}"))
        }
        None => host
            .default_input_device()
            .ok_or_else(|| "既定のオーディオ入力デバイスがありません".to_string()),
    }
}

/// 録音中の入力。`stop` で止めて、取り込んだ音を受け取る。
/// cpal のストリームはスレッドをまたげないので、専用のスレッドで持つ。
pub struct Recorder {
    stop_tx: mpsc::Sender<()>,
    handle: JoinHandle<Result<Raw, String>>,
}

impl Recorder {
    /// 録音を始める。`max` を過ぎると止める (その後 `stop` を呼べば、そこまでの音を返す)。
    pub fn start(device_id: Option<&str>, max: Duration) -> Result<Self, String> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let device_id = device_id.map(str::to_string);
        let handle = std::thread::spawn(move || {
            let opened = open(device_id.as_deref());
            let (stream, buffer, err_rx, channels, sample_rate) = match opened {
                Ok(o) => {
                    let _ = ready_tx.send(Ok(()));
                    o
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.clone()));
                    return Err(e);
                }
            };
            let _ = stop_rx.recv_timeout(max);
            drop(stream);
            if let Ok(err) = err_rx.try_recv() {
                return Err(format!("録音中にエラーが起きました: {err}"));
            }
            let samples = std::mem::take(&mut *buffer.lock().map_err(|e| e.to_string())?);
            Ok(Raw {
                samples,
                channels,
                sample_rate,
            })
        });
        ready_rx
            .recv()
            .map_err(|_| "録音のスレッドが応答しません".to_string())??;
        Ok(Self { stop_tx, handle })
    }

    pub fn stop(self) -> Result<Raw, String> {
        let _ = self.stop_tx.send(());
        self.handle
            .join()
            .map_err(|_| "録音のスレッドが異常終了しました".to_string())?
    }
}

/// 指定した秒数だけ録音する (自動ループ用)。
pub fn record(device_id: Option<&str>, seconds: u32) -> Result<Raw, String> {
    sound::check_duration(seconds)?;
    let recorder = Recorder::start(device_id, Duration::from_secs(MAX_RECORD_SECONDS as u64))?;
    std::thread::sleep(Duration::from_secs(seconds as u64));
    recorder.stop()
}

type Opened = (
    cpal::Stream,
    Arc<Mutex<Vec<f32>>>,
    mpsc::Receiver<String>,
    u16,
    u32,
);

fn open(device_id: Option<&str>) -> Result<Opened, String> {
    let device = find_device(device_id)?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("入力デバイス「{device}」の設定を取得できません: {e}"))?;
    let channels = supported.channels();
    let sample_rate = supported.sample_rate();
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    // 録音中に配列を伸ばすと、オーディオのスレッドで数 MB のコピーが起きて処理が間に合わなくなる
    // (macOS が過負荷 = Xrun を知らせる)。上限の長さの分を先に確保しておく
    let capacity = sample_rate as usize * channels as usize * (MAX_RECORD_SECONDS as usize + 1);
    let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(capacity)));
    let (err_tx, err_rx) = mpsc::channel::<String>();

    let stream = match format {
        SampleFormat::I8 => build::<i8>(&device, &config, &buffer, err_tx),
        SampleFormat::I16 => build::<i16>(&device, &config, &buffer, err_tx),
        SampleFormat::I32 => build::<i32>(&device, &config, &buffer, err_tx),
        SampleFormat::U8 => build::<u8>(&device, &config, &buffer, err_tx),
        SampleFormat::U16 => build::<u16>(&device, &config, &buffer, err_tx),
        SampleFormat::F32 => build::<f32>(&device, &config, &buffer, err_tx),
        SampleFormat::F64 => build::<f64>(&device, &config, &buffer, err_tx),
        other => return Err(format!("対応していないサンプル形式です: {other}")),
    }?;
    stream
        .play()
        .map_err(|e| format!("録音を開始できません: {e}"))?;
    Ok((stream, buffer, err_rx, channels, sample_rate))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: &Arc<Mutex<Vec<f32>>>,
    err_tx: mpsc::Sender<String>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let buffer = buffer.clone();
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _: &_| {
                if let Ok(mut buf) = buffer.lock() {
                    buf.extend(data.iter().map(|s| s.to_sample::<f32>()));
                }
            },
            move |err: cpal::Error| {
                if stops_recording(err.kind()) {
                    let _ = err_tx.send(err.to_string());
                } else {
                    eprintln!("録音中の警告 (録音は続ける): {err}");
                }
            },
            None,
        )
        .map_err(|e| format!("録音ストリームを作れません: {e}"))
}

/// 録音を失敗にするストリームのエラーか。音が一瞬途切れたかもしれない (Xrun)、
/// 実時間の優先度を得られなかった、経路が切り替わった、は録音を続けられるので止めない。
pub fn stops_recording(kind: cpal::ErrorKind) -> bool {
    !matches!(
        kind,
        cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::ErrorKind;

    #[test]
    fn glitches_do_not_stop_recording() {
        assert!(!stops_recording(ErrorKind::Xrun));
        assert!(!stops_recording(ErrorKind::RealtimeDenied));
        assert!(!stops_recording(ErrorKind::DeviceChanged));
    }

    #[test]
    fn losing_the_device_stops_recording() {
        for k in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::PermissionDenied,
            ErrorKind::StreamInvalidated,
            ErrorKind::BackendError,
            ErrorKind::Other,
        ] {
            assert!(stops_recording(k), "{k:?}");
        }
    }
}
