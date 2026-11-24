//! 実際の AI API を呼ぶ結合テスト。課金が発生するので既定では走らない。
//!
//! cargo test ai_live -- --ignored --nocapture

use crate::ai::{self, AiResponse, RequestInput};
use crate::config::{self, StoredSettings};
use crate::sound;

const SAMPLE_CHAIN: &str = r#"プリセット: 37B New Preset (Tempo: 70.4 BPM)

[block2] Tone Sovereign [ON]
  - Gain 1: 2.2 (0.0〜10.0)
  - Tone 1: 6.6 (0.0〜10.0)
  - Clipping 1: Boost [Overdrive / Boost / Distortion]
  - Level 1: 5.0 (0.0〜10.0)

[block5] Brit J45 Brt [ON]
  - Drive: 4.6 (0.0〜10.0)
  - Bass: 5.0 (0.0〜10.0)
  - Mid: 5.0 (0.0〜10.0)
  - Treble: 7.2 (0.0〜10.0)
  - Presence: 5.0 (0.0〜10.0)
  - Master: 10.0 (0.0〜10.0)

[block7] Simple Delay [ON]
  - Time: 500 ms (0 ms〜8000 ms)
  - Feedback: 30 % (0 %〜100 %)
  - Mix: 20 % (0 %〜100 %)
"#;

/// 高域の強いのこぎり波 (耳に痛い音の代わり)。アプリと同じく 16kHz モノラルにして送る。
fn bright_tone_wav() -> Vec<u8> {
    let sr = 48_000u32;
    let mut samples = Vec::new();
    for i in 0..sr * 3 {
        let t = i as f32 / sr as f32;
        let phase = (t * 220.0).fract();
        let s = (phase * 2.0 - 1.0) * 0.5;
        samples.push(s);
        samples.push(s);
    }
    sound::prepare_recording(&sound::Raw {
        samples,
        channels: 2,
        sample_rate: sr,
    })
    .unwrap()
    .wav
}

async fn ask(instruction: &str, recording: Option<&[u8]>) -> AiResponse {
    let _ = dotenvy::dotenv();
    let settings = StoredSettings::default()
        .resolve(&config::process_env)
        .expect("OPENAI_API_KEY が必要");
    let body = ai::build_request(
        &settings.model,
        &RequestInput {
            instruction,
            history: &[],
            reference_audio: None,
            recording_wav: recording,
            chain_text: SAMPLE_CHAIN,
            changed_parameters: &[],
        },
    );
    let started = std::time::Instant::now();
    let resp = ai::call(&settings, &body, &|m| println!("{m}"))
        .await
        .expect("API 呼び出し");
    println!(
        "model={} elapsed={:.1}s\n{}",
        settings.model,
        started.elapsed().as_secs_f64(),
        serde_json::to_string_pretty(&resp).unwrap()
    );
    resp
}

#[tokio::test]
#[ignore]
async fn ai_live_adjust_lowers_treble() {
    let wav = bright_tone_wav();
    let resp = ask(
        "高音が耳に痛い。アンプの Treble だけを少し下げて",
        Some(&wav),
    )
    .await;
    ai::validate(&resp).expect("検証");
    let treble = resp
        .changes
        .iter()
        .find(|c| c.block == "block5" && c.parameter.to_lowercase().contains("treble"))
        .expect("block5 の Treble の変更が無い");
    let next: f64 = treble.value.trim().parse().expect("数値でない");
    assert!(next < 7.2, "Treble が下がっていない: {next}");
}

#[tokio::test]
#[ignore]
async fn ai_live_adjusts_delay_mix_directly() {
    let wav = bright_tone_wav();
    let resp = ask("ディレイの Mix だけを 35 % にして", Some(&wav)).await;
    ai::validate(&resp).expect("検証");
    let mix = resp
        .changes
        .iter()
        .find(|c| c.block == "block7" && c.parameter.eq_ignore_ascii_case("Mix"))
        .expect("block7 の Mix の変更が無い");
    let val = mix.value.trim().trim_end_matches('%').trim();
    assert_eq!(val.parse::<f64>().unwrap(), 35.0);
}

#[tokio::test]
#[ignore]
async fn ai_live_recommends_a_model_from_the_catalog() {
    let wav = bright_tone_wav();
    let resp = ask(
        "歪みを Bluesbreaker 系 (BB 系) のオーバードライブに変えたい。どのモデルがよいか教えて。値は変えなくてよい",
        Some(&wav),
    )
    .await;
    ai::validate(&resp).expect("検証");
    assert!(
        ["Heir Apparent", "Tone Sovereign"]
            .iter()
            .any(|m| resp.explanation.contains(m)),
        "一覧のモデル名を勧めていない: {}",
        resp.explanation
    );
}
