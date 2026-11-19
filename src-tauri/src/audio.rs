//! オーディオ入力 (HX Stomp など) から指定秒数を録音し、WAV のバイト列にする。

use std::io::Cursor;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use serde::Serialize;

/// これより小さいピークの録音は無音とみなしてエラーにする (dBFS)。
pub const SILENCE_THRESHOLD_DBFS: f32 = -50.0;

/// 録音して WAV に入れるチャンネル数の上限。HX Stomp の USB 入力 1/2 (メイン L/R) を想定する。
pub const MAX_OUTPUT_CHANNELS: u16 = 2;

#[derive(Debug, Clone, Serialize)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct Recording {
    pub wav: Vec<u8>,
    pub peak_dbfs: f32,
    pub sample_rate: u32,
    pub channels: u16,
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

/// 指定したデバイスから `seconds` 秒録音する。
pub fn record(device_id: Option<&str>, seconds: u32) -> Result<Recording, String> {
    check_duration(seconds)?;
    let device = find_device(device_id)?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("入力デバイス「{device}」の設定を取得できません: {e}"))?;
    let in_channels = supported.channels();
    let sample_rate = supported.sample_rate();
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
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
    std::thread::sleep(Duration::from_secs(seconds as u64));
    drop(stream);

    if let Ok(err) = err_rx.try_recv() {
        return Err(format!("録音中にエラーが起きました: {err}"));
    }
    let samples = std::mem::take(&mut *buffer.lock().map_err(|e| e.to_string())?);
    if samples.is_empty() {
        return Err("録音データが空です。マイクの権限と入力デバイスを確かめてください".to_string());
    }

    let out_channels = in_channels.clamp(1, MAX_OUTPUT_CHANNELS);
    let selected = select_channels(&samples, in_channels, out_channels);
    let peak = peak_dbfs(&selected);
    check_not_silent(peak)?;
    let wav = encode_wav(&selected, out_channels, sample_rate)?;
    Ok(Recording {
        wav,
        peak_dbfs: peak,
        sample_rate,
        channels: out_channels,
    })
}

pub fn check_duration(seconds: u32) -> Result<(), String> {
    if (1..=30).contains(&seconds) {
        Ok(())
    } else {
        Err(format!("録音時間は 1〜30 秒にしてください: {seconds}"))
    }
}

/// 無音の録音を AI に送っても意味が無いので、エラーにする。
pub fn check_not_silent(peak_dbfs: f32) -> Result<(), String> {
    if peak_dbfs >= SILENCE_THRESHOLD_DBFS {
        Ok(())
    } else {
        Err(format!(
            "録音がほぼ無音でした (ピーク {peak_dbfs:.1} dBFS)。ギターを弾きながら録音し、入力デバイスが正しいか確かめてください"
        ))
    }
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
            move |err| {
                let _ = err_tx.send(err.to_string());
            },
            None,
        )
        .map_err(|e| format!("録音ストリームを作れません: {e}"))
}

/// インターリーブされたサンプルから先頭 `out_channels` 個のチャンネルだけを取り出す。
pub fn select_channels(samples: &[f32], in_channels: u16, out_channels: u16) -> Vec<f32> {
    let in_ch = in_channels.max(1) as usize;
    let out_ch = (out_channels as usize).min(in_ch).max(1);
    if in_ch == out_ch {
        return samples.to_vec();
    }
    samples
        .chunks_exact(in_ch)
        .flat_map(|frame| frame[..out_ch].iter().copied())
        .collect()
}

pub fn peak_dbfs(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * peak.log10()
    }
}

/// 16bit PCM の WAV にする。
pub fn encode_wav(samples: &[f32], channels: u16, sample_rate: u32) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| format!("WAV を作れません: {e}"))?;
        for &s in samples {
            writer
                .write_sample(s.clamp(-1.0, 1.0).to_sample::<i16>())
                .map_err(|e| format!("WAV を書けません: {e}"))?;
        }
        writer
            .finalize()
            .map_err(|e| format!("WAV を書けません: {e}"))?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_first_two_of_six_channels() {
        // 6ch × 2 フレーム
        let s = [
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
        ];
        assert_eq!(select_channels(&s, 6, 2), vec![1.0, 2.0, 11.0, 12.0]);
    }

    #[test]
    fn select_keeps_mono() {
        assert_eq!(select_channels(&[0.1, 0.2], 1, 2), vec![0.1, 0.2]);
    }

    #[test]
    fn peak_in_dbfs() {
        assert!((peak_dbfs(&[0.0, -0.5, 0.25]) - (-6.0206)).abs() < 0.01);
        assert_eq!(peak_dbfs(&[0.0, 0.0]), f32::NEG_INFINITY);
    }

    #[test]
    fn duration_range() {
        assert!(check_duration(0).is_err());
        assert!(check_duration(1).is_ok());
        assert!(check_duration(30).is_ok());
        assert!(check_duration(31).is_err());
    }

    #[test]
    fn silence_threshold() {
        assert!(check_not_silent(-12.0).is_ok());
        assert!(check_not_silent(SILENCE_THRESHOLD_DBFS).is_ok());
        assert!(check_not_silent(SILENCE_THRESHOLD_DBFS - 0.1).is_err());
        assert!(check_not_silent(f32::NEG_INFINITY).is_err(), "完全な無音");
        // ギターの普通の録音 (-30 dBFS 程度) を無音と誤判定しない
        assert!(check_not_silent(-30.0).is_ok());
    }

    #[test]
    fn wav_clips_out_of_range_samples() {
        let wav = encode_wav(&[2.0, -2.0, 0.0], 1, 48_000).unwrap();
        let samples: Vec<i16> = hound::WavReader::new(Cursor::new(wav))
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(samples[0], i16::MAX);
        assert!(
            samples[1] <= -i16::MAX,
            "負の側も飽和させる: {}",
            samples[1]
        );
        assert_eq!(samples[2], 0);
    }

    #[test]
    fn wav_roundtrip() {
        let samples: Vec<f32> = (0..480)
            .map(|i| (i as f32 / 48.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        let wav = encode_wav(&samples, 2, 48_000).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        let reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(reader.len() as usize, samples.len());
    }
}
