//! 実際の AI API を呼ぶ結合テスト。課金が発生するので既定では走らない。
//!
//! cargo test ai_live -- --ignored --nocapture
//!
//! 入力の画面は scripts/make_mock_screenshot.swift で描いた HX Edit 風の模擬画面
//! (1200×800 ポイント) である。数値欄の中心は x = 100 + 140 * i, y = 712 ポイントにある。
//! GP-5 は scripts/make_mock_valeton.swift で描いた Valeton Suite 風の模擬画面
//! (1000×773 ポイント) である。スライダーは x 487〜938、i 本目の縦は 165 + 42 * i から高さ 30。

use crate::ai::{self, ActionKind, AiResponse, Purpose, RequestInput, RestoreTarget};
use crate::config::{self, StoredSettings};
use crate::device::Device;
use crate::geometry::{MAX_SCREENSHOT_SIDE, fit_within};
use crate::sound;

const SCREENSHOT: &[u8] = include_bytes!("../tests/fixtures/mock_hx_edit.png");
const VALETON: &[u8] = include_bytes!("../tests/fixtures/mock_valeton.png");

/// アプリと同じく、長辺を `MAX_SCREENSHOT_SIDE` に縮めたスクリーンショット。
fn screenshot(original: &[u8]) -> (Vec<u8>, (u32, u32)) {
    let image = image::load_from_memory(original).unwrap();
    let (w, h) = fit_within(image.width(), image.height(), MAX_SCREENSHOT_SIDE);
    let resized = image.resize_exact(w, h, image::imageops::FilterType::Triangle);
    let mut png = Vec::new();
    resized
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    (png, (w, h))
}

/// 模擬画面の要素の中心 (正規化座標)。
const TREBLE_VALUE: (f64, f64) = (520.0 / 1200.0 * 1000.0, 712.0 / 800.0 * 1000.0);
const DELAY_BLOCK: (f64, f64) = (696.0 / 1200.0 * 1000.0, 152.0 / 800.0 * 1000.0);

/// 正解からこの距離 (正規化座標) 以内なら当たりとみなす。数値欄の半幅は約 28 である。
const TOLERANCE: f64 = 30.0;

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

async fn ask(purpose: Purpose<'_>, recording: Option<&[u8]>) -> AiResponse {
    ask_on(Device::HxStomp, SCREENSHOT, purpose, recording).await
}

async fn ask_on(
    device: Device,
    original: &[u8],
    purpose: Purpose<'_>,
    recording: Option<&[u8]>,
) -> AiResponse {
    let _ = dotenvy::dotenv();
    let mut settings = StoredSettings::default()
        .resolve(&config::process_env)
        .expect("OPENAI_API_KEY が必要");
    settings.device = device;
    let (png, size) = screenshot(original);
    let body = ai::build_request(
        &settings.model,
        &RequestInput {
            device,
            purpose,
            history: &[],
            reference_audio: None,
            recording_wav: recording,
            screenshot_png: &png,
            screenshot_size: size,
            round: 1,
            max_rounds: 4,
            done_in_this_cycle: &[],
            earlier_explanations: &[],
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

fn near(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() <= TOLERANCE && (a.1 - b.1).abs() <= TOLERANCE
}

#[tokio::test]
#[ignore]
async fn ai_live_adjust_reads_treble_field() {
    let wav = bright_tone_wav();
    let purpose = Purpose::Adjust {
        instruction: "高音が耳に痛い。アンプの Treble だけを少し下げて",
    };
    let resp = ask(purpose.clone(), Some(&wav)).await;
    ai::validate(&resp, &purpose, false, Device::HxStomp).expect("検証");
    let treble = resp
        .actions
        .iter()
        .find(|a| a.kind == ActionKind::SetValue && a.parameter.to_lowercase().contains("treble"))
        .expect("Treble の変更が無い");
    assert_eq!(treble.previous_value.trim(), "7.2", "変更前の値の読み取り");
    assert!(
        treble.value.trim().parse::<f64>().unwrap() < 7.2,
        "Treble が下がっていない"
    );
    assert!(
        near((treble.x, treble.y), TREBLE_VALUE),
        "Treble の座標 ({}, {}) が正解 {:?} から外れている",
        treble.x,
        treble.y,
        TREBLE_VALUE
    );
}

#[tokio::test]
#[ignore]
async fn ai_live_revert_types_previous_value() {
    let targets = vec![RestoreTarget {
        block: "Amp".into(),
        parameter: "Treble".into(),
        value: "6.0".into(),
    }];
    let purpose = Purpose::Revert { targets: &targets };
    let resp = ask(purpose.clone(), None).await;
    ai::validate(&resp, &purpose, false, Device::HxStomp).expect("検証");
    let set: Vec<_> = resp
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::SetValue)
        .collect();
    assert_eq!(set.len(), 1, "戻す値は 1 つだけ");
    assert_eq!(set[0].value.trim(), "6.0");
    assert!(
        near((set[0].x, set[0].y), TREBLE_VALUE),
        "座標 ({}, {}) が正解 {:?} から外れている",
        set[0].x,
        set[0].y,
        TREBLE_VALUE
    );
}

#[tokio::test]
#[ignore]
async fn ai_live_selects_hidden_block() {
    let wav = bright_tone_wav();
    let purpose = Purpose::Adjust {
        instruction: "ディレイの Mix を少し上げて。アンプは触らないで",
    };
    let resp = ask(purpose.clone(), Some(&wav)).await;
    ai::validate(&resp, &purpose, false, Device::HxStomp).expect("検証");
    let last = resp.actions.last().expect("操作が無い");
    assert_eq!(
        last.kind,
        ActionKind::SelectBlock,
        "最後にブロック選択が来るはず"
    );
    assert!(resp.needs_another_round);
    assert!(
        near((last.x, last.y), DELAY_BLOCK),
        "Delay ブロックの座標 ({}, {}) が正解 {:?} から外れている",
        last.x,
        last.y,
        DELAY_BLOCK
    );
}

#[tokio::test]
#[ignore]
async fn ai_live_recommends_a_model_from_the_catalog() {
    let wav = bright_tone_wav();
    let purpose = Purpose::Adjust {
        instruction: "歪みを Bluesbreaker 系 (BB 系) のオーバードライブに変えたい。どのモデルがよいか教えて。値は変えなくてよい",
    };
    let resp = ask(purpose.clone(), Some(&wav)).await;
    ai::validate(&resp, &purpose, false, Device::HxStomp).expect("検証");
    // 一覧で Bluesbreaker 系に分類した HX Stomp のモデル
    assert!(
        ["Heir Apparent", "Tone Sovereign"]
            .iter()
            .any(|m| resp.explanation.contains(m)),
        "一覧のモデル名を勧めていない: {}",
        resp.explanation
    );
}

#[tokio::test]
#[ignore]
async fn ai_live_gp5_points_at_the_treble_slider_with_its_range() {
    let wav = bright_tone_wav();
    let purpose = Purpose::Adjust {
        instruction: "高音が耳に痛い。AMP の Treble だけを少し下げて",
    };
    let resp = ask_on(Device::Gp5, VALETON, purpose.clone(), Some(&wav)).await;
    ai::validate(&resp, &purpose, false, Device::Gp5).expect("検証");
    let treble = resp
        .actions
        .iter()
        .find(|a| a.kind == ActionKind::SetValue && a.parameter.to_lowercase().contains("treble"))
        .expect("Treble の変更が無い");
    assert_eq!(treble.previous_value.trim(), "65", "変更前の値の読み取り");
    assert_eq!((treble.min, treble.max), (Some(0.0), Some(100.0)), "範囲");
    assert!(treble.value.trim().parse::<f64>().unwrap() < 65.0);
    // Treble のバー (x 487〜938、y 375〜405 ポイント) の上を指しているか
    let (x, y) = (treble.x / 1000.0 * 1000.0, treble.y / 1000.0 * 773.0);
    assert!((487.0..=938.0).contains(&x), "x {x}");
    assert!((375.0..=405.0).contains(&y), "y {y}");
    // その点で画素を読むと、アプリの安全確認 (塗りの位置と変更前の値) を通る
    let img = image::load_from_memory(VALETON).unwrap().to_rgba8();
    let s =
        crate::slider::read(&img, (x * 2.0) as u32, (y * 2.0) as u32).expect("スライダーを読む");
    assert!(
        (s.fraction() - 0.65).abs() <= crate::automation::PREVIOUS_TOLERANCE,
        "{}",
        s.fraction()
    );
}
