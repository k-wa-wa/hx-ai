//! 録音やリファレンス音源を AI に送る形 (16kHz モノラルの WAV) に整える。OS には触らない。

use std::io::Cursor;
use std::ops::Range;

use cpal::Sample;

/// これより小さいピークの録音は無音とみなしてエラーにする (dBFS)。
pub const SILENCE_THRESHOLD_DBFS: f32 = -50.0;

/// 録音の長さの上限 (秒)。手動の録音はここで自動的に止まる。
pub const MAX_RECORD_SECONDS: u32 = 30;

/// AI に送る音のサンプリング周波数。Gemini は音声を 16kHz に落として聴くので、
/// それより細かく送ってもデータが増えるだけである。
pub const AI_SAMPLE_RATE: u32 = 16_000;

/// 無音を削るときの閾値。録音のピークからこれだけ下までを音とみなす (dB)。
pub const TRIM_BELOW_PEAK_DB: f32 = 40.0;
/// 無音を削るときの閾値の下限 (dBFS)。ノイズを音とみなさないため。
pub const TRIM_FLOOR_DBFS: f32 = -60.0;
/// 音の出だしの前に残す長さ (秒)。アタックを削らないため。
pub const TRIM_PRE_ROLL: f32 = 0.05;
/// 音の終わりの後に残す長さ (秒)。リバーブやディレイの余韻を残すため。
pub const TRIM_POST_ROLL: f32 = 0.3;

/// 入力デバイスから取り込んだそのままの音。
#[derive(Debug, Clone)]
pub struct Raw {
    /// チャンネルが交互に並んだサンプル。
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

/// AI に送れる形に整えた録音。
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    /// 16kHz モノラル 16bit の WAV。
    pub wav: Vec<u8>,
    pub peak_dbfs: f32,
    /// 無音を削った後の長さ。
    pub seconds: f32,
}

pub fn check_duration(seconds: u32) -> Result<(), String> {
    if (1..=MAX_RECORD_SECONDS).contains(&seconds) {
        Ok(())
    } else {
        Err(format!(
            "録音時間は 1〜{MAX_RECORD_SECONDS} 秒にしてください: {seconds}"
        ))
    }
}

/// 無音の録音を AI に送っても意味が無いので、エラーにする。
/// ピークがこれだけ小さければ無音とみなす。録音とリファレンスで同じ基準を使う。
pub fn is_silent(peak_dbfs: f32) -> bool {
    peak_dbfs < SILENCE_THRESHOLD_DBFS
}

pub fn check_not_silent(peak_dbfs: f32) -> Result<(), String> {
    if !is_silent(peak_dbfs) {
        Ok(())
    } else {
        Err(format!(
            "録音がほぼ無音でした (ピーク {peak_dbfs:.1} dBFS)。ギターを弾きながら録音し、入力デバイスが正しいか確かめてください"
        ))
    }
}

pub fn peak_dbfs(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * peak.log10()
    }
}

/// 先頭 2 チャンネル (HX Stomp の USB 入力 1/2 = メイン L/R) を平均してモノラルにする。
/// 3 チャンネル目以降 (リアンプ用のドライ音など) は使わない。
pub fn mix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    let used = ch.min(2);
    samples
        .chunks_exact(ch)
        .map(|frame| frame[..used].iter().sum::<f32>() / used as f32)
        .collect()
}

/// 音が鳴っている区間を返す。前後に `TRIM_PRE_ROLL` / `TRIM_POST_ROLL` の余白を付ける。
/// 全体が無音なら空の区間を返す。
pub fn trim_silence(mono: &[f32], sample_rate: u32) -> Range<usize> {
    let peak = peak_dbfs(mono);
    if !peak.is_finite() {
        return 0..0;
    }
    let threshold_db = (peak - TRIM_BELOW_PEAK_DB).max(TRIM_FLOOR_DBFS);
    let threshold = 10f32.powf(threshold_db / 20.0);
    let loud = |s: &f32| s.abs() >= threshold;
    let (Some(first), Some(last)) = (mono.iter().position(loud), mono.iter().rposition(loud))
    else {
        return 0..0;
    };
    let pre = (TRIM_PRE_ROLL * sample_rate as f32) as usize;
    let post = (TRIM_POST_ROLL * sample_rate as f32) as usize;
    first.saturating_sub(pre)..(last + 1 + post).min(mono.len())
}

/// 帯域を制限してからサンプリング周波数を変える。落とすときに高域が折り返して
/// 雑音にならないよう、先にローパスをかける。
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let kernel = lowpass_kernel(0.45 * from.min(to) as f64 / from as f64);
    let half = kernel.len() / 2;
    // 入力の位置 i をローパスに通した値 (範囲外は 0 とみなす)
    let filtered_at = |i: usize| -> f32 {
        let mut acc = 0.0f32;
        for (k, w) in kernel.iter().enumerate() {
            let j = (i + k).checked_sub(half);
            if let Some(&x) = j.and_then(|j| input.get(j)) {
                acc += x * w;
            }
        }
        acc
    };
    let out_len = (input.len() as f64 / ratio).floor() as usize;
    (0..out_len)
        .map(|n| {
            let pos = n as f64 * ratio;
            let i = pos.floor() as usize;
            let frac = (pos - i as f64) as f32;
            let a = filtered_at(i);
            if frac == 0.0 {
                a
            } else {
                a + (filtered_at(i + 1) - a) * frac
            }
        })
        .collect()
}

/// Hann 窓をかけた sinc のローパス。`cutoff` は入力のサンプリング周波数に対する比。
/// 窓をかけた sinc の総和はほぼ 1 なので、正規化はしない (直流の利得はテストで確かめる)。
fn lowpass_kernel(cutoff: f64) -> Vec<f32> {
    const TAPS: usize = 63;
    let m = (TAPS - 1) as f64 / 2.0;
    (0..TAPS)
        .map(|n| {
            let x = n as f64 - m;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let window = 0.5 * (1.0 + (std::f64::consts::PI * x / m).cos());
            (sinc * window) as f32
        })
        .collect()
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

/// 録音をモノラルにし、前後の無音を削り、16kHz に落として WAV にする。
pub fn prepare_recording(raw: &Raw) -> Result<Prepared, String> {
    if raw.samples.is_empty() {
        return Err("録音データが空です。マイクの権限と入力デバイスを確かめてください".to_string());
    }
    let mono = mix_to_mono(&raw.samples, raw.channels);
    let peak = peak_dbfs(&mono);
    check_not_silent(peak)?;
    let range = trim_silence(&mono, raw.sample_rate);
    let resampled = resample(&mono[range], raw.sample_rate, AI_SAMPLE_RATE);
    Ok(Prepared {
        wav: encode_wav(&resampled, 1, AI_SAMPLE_RATE)?,
        peak_dbfs: peak,
        seconds: resampled.len() as f32 / AI_SAMPLE_RATE as f32,
    })
}

/// リファレンス音源のうち、AI に送る区間の長さ (秒)。曲全体を毎回送らないため。
pub const REFERENCE_CLIP_SECONDS: f32 = 20.0;

/// 読み込んだリファレンス音源 (元のサンプリング周波数のモノラル)。
#[derive(Debug, Clone)]
pub struct ReferenceTrack {
    mono: Vec<f32>,
    sample_rate: u32,
}

/// リファレンスから切り出した、AI に送る区間。
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceClip {
    /// 16kHz モノラル 16bit の WAV。
    pub wav: Vec<u8>,
    pub start: f32,
    pub seconds: f32,
}

impl ReferenceTrack {
    pub fn new(raw: &Raw) -> Result<Self, String> {
        if raw.samples.is_empty() || raw.sample_rate == 0 {
            return Err("音源に音が入っていません".to_string());
        }
        Ok(Self {
            mono: mix_to_mono(&raw.samples, raw.channels),
            sample_rate: raw.sample_rate,
        })
    }

    /// WAV / MP3 のバイト列を読む。`extension` は形式を見分ける手がかり ("wav" / "mp3")。
    pub fn decode(bytes: &[u8], extension: &str) -> Result<Self, String> {
        Self::new(&decode(bytes, extension)?)
    }

    pub fn duration(&self) -> f32 {
        self.mono.len() as f32 / self.sample_rate as f32
    }

    /// 開始位置を、区間が音源に収まる範囲に寄せる。
    pub fn clamp_start(&self, start: f32) -> f32 {
        let latest = (self.duration() - REFERENCE_CLIP_SECONDS).max(0.0);
        if start.is_nan() {
            0.0
        } else {
            start.clamp(0.0, latest)
        }
    }

    /// `start` 秒から `REFERENCE_CLIP_SECONDS` 秒を切り出し、16kHz モノラルの WAV にする。
    pub fn clip(&self, start: f32) -> Result<ReferenceClip, String> {
        let start = self.clamp_start(start);
        let rate = self.sample_rate as f32;
        let from = ((start * rate) as usize).min(self.mono.len());
        let to = (from + (REFERENCE_CLIP_SECONDS * rate) as usize).min(self.mono.len());
        let segment = &self.mono[from..to];
        let peak = peak_dbfs(segment);
        if is_silent(peak) {
            return Err(format!(
                "リファレンスの {} からの区間がほぼ無音です (ピーク {peak:.1} dBFS)。再生位置を変えて選び直してください",
                format_time(start)
            ));
        }
        let resampled = resample(segment, self.sample_rate, AI_SAMPLE_RATE);
        Ok(ReferenceClip {
            wav: encode_wav(&resampled, 1, AI_SAMPLE_RATE)?,
            start,
            seconds: segment.len() as f32 / rate,
        })
    }
}

/// 秒を「分:秒」で書く (エラーの文言用)。
fn format_time(seconds: f32) -> String {
    let s = seconds.max(0.0) as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// 音声ファイルのバイト列をデコードする。
pub fn decode(bytes: &[u8], extension: &str) -> Result<Raw, String> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let fail = |e: &dyn std::fmt::Display| format!("音源ファイルを読めません: {e}");
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| fail(&e))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "音源ファイルに音声のトラックがありません".to_string())?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "音源ファイルの形式が分かりません".to_string())?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| fail(&e))?;

    let mut samples = Vec::new();
    let mut chunk: Vec<f32> = Vec::new();
    let mut spec: Option<(u16, u32)> = None;
    while let Some(packet) = format.next_packet().map_err(|e| fail(&e))? {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let s = buf.spec();
                spec.get_or_insert((s.channels().count() as u16, s.rate()));
                buf.copy_to_vec_interleaved(&mut chunk);
                samples.extend_from_slice(&chunk);
            }
            // 壊れたフレームは飛ばして続ける
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(fail(&e)),
        }
    }
    let (channels, sample_rate) =
        spec.ok_or_else(|| "音源ファイルに音が入っていません".to_string())?;
    Ok(Raw {
        samples,
        channels,
        sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, seconds: f32, amp: f32) -> Vec<f32> {
        (0..(rate as f32 * seconds) as usize)
            .map(|i| (i as f32 / rate as f32 * freq * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    fn read_wav(wav: &[u8]) -> (hound::WavSpec, Vec<i16>) {
        let reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
        let spec = reader.spec();
        let samples = reader.into_samples::<i16>().map(Result::unwrap).collect();
        (spec, samples)
    }

    /// 定常部分 (端の過渡を除く) のピーク。
    fn steady_peak(s: &[f32]) -> f32 {
        let n = s.len();
        s[n / 4..n * 3 / 4]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn duration_range() {
        assert!(check_duration(0).is_err());
        assert!(check_duration(1).is_ok());
        assert!(check_duration(MAX_RECORD_SECONDS).is_ok());
        assert!(check_duration(MAX_RECORD_SECONDS + 1).is_err());
        assert_eq!(MAX_RECORD_SECONDS, 30);
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
    fn peak_in_dbfs() {
        assert!((peak_dbfs(&[0.0, -0.5, 0.25]) - (-6.0206)).abs() < 0.01);
        assert_eq!(peak_dbfs(&[0.0, 0.0]), f32::NEG_INFINITY);
    }

    #[test]
    fn mono_averages_first_two_of_six_channels() {
        // 6ch × 2 フレーム。3 チャンネル目以降は使わない
        let s = [0.2, 0.4, 9.0, 9.0, 9.0, 9.0, -0.2, 0.0, 9.0, 9.0, 9.0, 9.0];
        let m = mix_to_mono(&s, 6);
        assert_eq!(m.len(), 2);
        assert!((m[0] - 0.3).abs() < 1e-6);
        assert!((m[1] - -0.1).abs() < 1e-6);
    }

    #[test]
    fn mono_keeps_single_channel() {
        assert_eq!(mix_to_mono(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn trim_removes_leading_and_trailing_silence_with_margins() {
        let rate = 1000;
        let mut s = vec![0.0; 2000];
        s.extend(vec![0.5; 1000]); // 2.0〜3.0 秒に音
        s.extend(vec![0.0; 2000]);
        let r = trim_silence(&s, rate);
        assert_eq!(r.start, 2000 - 50, "出だしの前に 0.05 秒残す");
        assert_eq!(r.end, 3000 + 300, "終わりの後に 0.3 秒残す");
    }

    #[test]
    fn trim_keeps_quiet_tail_within_40db_of_peak() {
        let rate = 1000;
        let mut s = vec![0.0; 1000];
        s.extend(vec![0.5; 500]);
        // 余韻: ピーク (-6 dBFS) から 30dB 下 (-36 dBFS) は音として残す
        s.extend(vec![0.5 * 10f32.powf(-30.0 / 20.0); 500]);
        // ピークから 50dB 下は無音とみなす
        s.extend(vec![0.5 * 10f32.powf(-50.0 / 20.0); 1000]);
        let r = trim_silence(&s, rate);
        assert_eq!(r.end, 2000 + 300);
    }

    #[test]
    fn trim_does_not_treat_noise_below_floor_as_sound() {
        // 小さい音だけの録音でも、-60 dBFS 未満は音とみなさない
        let rate = 1000;
        let quiet = 10f32.powf(-45.0 / 20.0);
        let noise = 10f32.powf(-70.0 / 20.0);
        let mut s = vec![noise; 1000];
        s.extend(vec![quiet; 1000]);
        s.extend(vec![noise; 1000]);
        let r = trim_silence(&s, rate);
        assert_eq!(r, 950..2300);
    }

    #[test]
    fn trim_margins_are_clamped_to_the_recording() {
        let r = trim_silence(&[0.5, 0.5, 0.5], 1000);
        assert_eq!(r, 0..3);
    }

    #[test]
    fn trim_of_silence_is_empty() {
        assert!(trim_silence(&[0.0; 100], 1000).is_empty());
        assert!(trim_silence(&[], 1000).is_empty());
    }

    #[test]
    fn resample_changes_length_by_ratio() {
        assert_eq!(resample(&vec![0.0; 48_000], 48_000, 16_000).len(), 16_000);
        assert_eq!(resample(&vec![0.0; 44_100], 44_100, 16_000).len(), 16_000);
        assert_eq!(resample(&[0.1, 0.2], 16_000, 16_000), vec![0.1, 0.2]);
    }

    /// 正弦波を落とした結果を、新しい周波数で直接作った正弦波と 1 サンプルずつ比べる。
    /// 振幅だけでなく、時間のずれや符号の反転にも気づける。
    fn max_error_against_ideal(freq: f32, from: u32) -> f32 {
        let out = resample(&sine(freq, from, 0.5, 0.5), from, 16_000);
        let ideal = sine(freq, 16_000, 0.5, 0.5);
        let n = out.len();
        (n / 4..n * 3 / 4).fold(0.0f32, |m, i| m.max((out[i] - ideal[i]).abs()))
    }

    #[test]
    fn resample_matches_ideal_sine_in_audible_band() {
        for (freq, from) in [
            (1000.0, 48_000),
            (2000.0, 48_000),
            (5000.0, 48_000),
            (1000.0, 44_100),
            (2000.0, 44_100),
        ] {
            let e = max_error_against_ideal(freq, from);
            assert!(e < 0.01, "{from}Hz からの {freq}Hz の誤差 {e}");
        }
    }

    #[test]
    fn resample_keeps_dc_level() {
        for from in [48_000, 44_100] {
            let out = resample(&vec![0.5; from as usize], from, 16_000);
            let n = out.len();
            for v in &out[n / 4..n * 3 / 4] {
                assert!((v - 0.5).abs() < 0.001, "{from}Hz: {v}");
            }
        }
    }

    #[test]
    fn resample_removes_content_above_new_nyquist() {
        // 16kHz では 8kHz より上を表せない。ローパスが無いと低い音に折り返して残る
        for (freq, from) in [(12_000.0, 48_000), (9_000.0, 48_000), (10_000.0, 44_100)] {
            let out = resample(&sine(freq, from, 0.5, 0.5), from, 16_000);
            let p = steady_peak(&out);
            assert!(p < 0.005, "{from}Hz からの {freq}Hz が {p} 残った");
        }
    }

    #[test]
    fn wav_clips_out_of_range_samples() {
        let (_, samples) = read_wav(&encode_wav(&[2.0, -2.0, 0.0], 1, 48_000).unwrap());
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
        let samples = sine(1000.0, 48_000, 0.01, 0.5);
        let wav = encode_wav(&samples, 2, 48_000).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        let (spec, read) = read_wav(&wav);
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(read.len(), samples.len());
    }

    #[test]
    fn prepare_makes_trimmed_16khz_mono_wav() {
        // 48kHz ステレオ: 1 秒の無音 + 2 秒の音 + 1 秒の無音
        let tone = sine(440.0, 48_000, 2.0, 0.5);
        let mut mono = vec![0.0; 48_000];
        mono.extend(&tone);
        mono.extend(vec![0.0; 48_000]);
        let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
        let p = prepare_recording(&Raw {
            samples: stereo,
            channels: 2,
            sample_rate: 48_000,
        })
        .unwrap();

        let (spec, samples) = read_wav(&p.wav);
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 16_000);
        assert!(
            (p.seconds - 2.35).abs() < 0.01,
            "2 秒 + 前後の余白: {}",
            p.seconds
        );
        assert_eq!(samples.len(), (p.seconds * 16_000.0).round() as usize);
        assert!((p.peak_dbfs - -6.02).abs() < 0.05, "{}", p.peak_dbfs);
    }

    #[test]
    fn prepare_rejects_silence_and_empty() {
        let silent = Raw {
            samples: vec![0.0001; 4800],
            channels: 1,
            sample_rate: 48_000,
        };
        assert!(prepare_recording(&silent).unwrap_err().contains("無音"));
        let empty = Raw {
            samples: vec![],
            channels: 2,
            sample_rate: 48_000,
        };
        assert!(prepare_recording(&empty).unwrap_err().contains("空"));
    }

    #[test]
    fn ai_sample_rate_is_16khz() {
        assert_eq!(AI_SAMPLE_RATE, 16_000);
    }

    fn track(seconds: f32, rate: u32) -> ReferenceTrack {
        // 1 秒ごとに音量が上がる (どこを切り出したかを音量で見分けられる)
        let samples = (0..(seconds * rate as f32) as usize)
            .map(|i| {
                let level = 0.01 * (1 + i / rate as usize) as f32;
                (i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin() * level
            })
            .collect();
        ReferenceTrack::new(&Raw {
            samples,
            channels: 1,
            sample_rate: rate,
        })
        .unwrap()
    }

    #[test]
    fn reference_clip_is_20s_of_16khz_mono_from_start() {
        let t = track(60.0, 8_000);
        assert_eq!(t.duration(), 60.0);
        let c = t.clip(30.0).unwrap();
        assert_eq!(c.start, 30.0);
        assert_eq!(c.seconds, 20.0);
        let (spec, samples) = read_wav(&c.wav);
        assert_eq!((spec.channels, spec.sample_rate), (1, 16_000));
        assert_eq!(samples.len(), 20 * 16_000);
        // 30〜50 秒目を切り出したので、音量は 0.31〜0.50 の間にある
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap() as f32 / 32768.0;
        assert!((0.48..=0.51).contains(&peak), "ピーク {peak}");
        let first = samples[16_000 / 4..16_000 / 2]
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap() as f32
            / 32768.0;
        assert!((0.30..=0.32).contains(&first), "出だしのピーク {first}");
    }

    #[test]
    fn reference_start_is_clamped_so_the_clip_fits() {
        let t = track(60.0, 8_000);
        assert_eq!(t.clamp_start(-5.0), 0.0);
        assert_eq!(t.clamp_start(55.0), 40.0);
        assert_eq!(t.clamp_start(f32::NAN), 0.0);
        assert_eq!(t.clip(55.0).unwrap().start, 40.0);
        assert_eq!(t.clip(55.0).unwrap().seconds, 20.0);
    }

    #[test]
    fn short_reference_is_sent_whole() {
        let t = track(5.0, 8_000);
        assert_eq!(t.clamp_start(3.0), 0.0);
        let c = t.clip(3.0).unwrap();
        assert_eq!((c.start, c.seconds), (0.0, 5.0));
    }

    #[test]
    fn silent_reference_clip_is_an_error_with_its_position() {
        let mut samples = vec![0.0f32; 8_000 * 70];
        samples[..8_000].fill(0.5);
        let t = ReferenceTrack::new(&Raw {
            samples,
            channels: 1,
            sample_rate: 8_000,
        })
        .unwrap();
        assert!(t.clip(0.0).is_ok());
        let e = t.clip(65.0).unwrap_err();
        assert!(e.contains("0:50 からの区間がほぼ無音"), "{e}");
    }

    #[test]
    fn decode_reads_wav_and_mixes_channels() {
        let stereo: Vec<f32> = (0..4410).flat_map(|_| [0.5, 0.25]).collect();
        let wav = encode_wav(&stereo, 2, 44_100).unwrap();
        let raw = decode(&wav, "wav").unwrap();
        assert_eq!((raw.channels, raw.sample_rate), (2, 44_100));
        assert_eq!(raw.samples.len(), 8820);
        let t = ReferenceTrack::decode(&wav, "wav").unwrap();
        assert!((t.duration() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"not audio", "mp3").is_err());
        assert!(
            ReferenceTrack::new(&Raw {
                samples: vec![],
                channels: 1,
                sample_rate: 8_000
            })
            .is_err()
        );
    }

    #[test]
    fn reference_clip_length_is_20s() {
        assert_eq!(REFERENCE_CLIP_SECONDS, 20.0);
    }
}
