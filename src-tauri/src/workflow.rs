//! 1 サイクル (録音 → キャプチャ → AI → 自動操作) と Undo の流れ。
//!
//! キャプチャ・AI・自動操作は `Engine` トレイトの向こうに置き、テストでは偽物に差し替える。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::ai::{
    self, ActionKind, AiAction, AiResponse, AudioFormat, ChatTurn, DoneAction, Purpose,
    RequestInput, RestoreTarget,
};
use crate::automation::{self, Outcome};
use crate::capture::{self, Screenshot, TargetWindow};
use crate::config::{self, ResolvedSettings, StoredSettings};
use crate::macos;
use crate::sound::{ReferenceClip, ReferenceTrack};

/// 1 サイクルで撮り直して AI に聞き直す回数の上限。
pub const MAX_ROUNDS: usize = 4;

/// リファレンス音源ファイルの上限。AI には切り出した区間しか送らないので、
/// 上限は読み込みとプレビューの重さで決める。数分の WAV (数十 MB) は受け付ける。
pub const MAX_REFERENCE_BYTES: usize = 50 * 1024 * 1024;
// 数分の WAV (44.1kHz ステレオで 1 分 約 10MB) を受け付けること
const _: () = assert!(MAX_REFERENCE_BYTES >= 50 * 1024 * 1024);

/// Undo 履歴に残す数。
pub const MAX_UNDO_GROUPS: usize = 20;

pub struct Reference {
    pub name: String,
    /// プレビュー再生用の元のファイル。
    pub bytes: Vec<u8>,
    pub format: AudioFormat,
    pub track: ReferenceTrack,
    /// AI に送る区間。
    pub clip: ReferenceClip,
}

impl Reference {
    /// 音源ファイルを読み、先頭からの区間を AI に送る区間にする。
    pub fn new(name: String, bytes: Vec<u8>, format: AudioFormat) -> Result<Self, String> {
        if bytes.len() > MAX_REFERENCE_BYTES {
            return Err(format!(
                "音源ファイルが大きすぎます ({:.1} MB)。{} MB 以下に切り出してください",
                bytes.len() as f64 / 1024.0 / 1024.0,
                MAX_REFERENCE_BYTES / 1024 / 1024
            ));
        }
        let track = ReferenceTrack::decode(&bytes, format.as_str())?;
        let clip = track.clip(0.0)?;
        Ok(Self {
            name,
            bytes,
            format,
            track,
            clip,
        })
    }

    /// AI に送る区間の開始位置を変える。無音の区間を選んだときは元の区間のまま、エラーにする。
    pub fn set_start(&mut self, seconds: f32) -> Result<(), String> {
        self.clip = self.track.clip(seconds)?;
        Ok(())
    }
}

pub struct AppState {
    pub settings_path: std::path::PathBuf,
    pub stored: Mutex<StoredSettings>,
    pub reference: Mutex<Option<Reference>>,
    /// Undo 用。1 要素が 1 サイクル分の「戻す値」の一覧。
    pub undo_stack: Mutex<Vec<Vec<RestoreTarget>>>,
    pub chat: Mutex<Vec<ChatTurn>>,
    pub busy: AtomicBool,
}

impl AppState {
    pub fn new(settings_path: std::path::PathBuf, stored: StoredSettings) -> Self {
        Self {
            settings_path,
            stored: Mutex::new(stored),
            reference: Mutex::new(None),
            undo_stack: Mutex::new(Vec::new()),
            chat: Mutex::new(Vec::new()),
            busy: AtomicBool::new(false),
        }
    }

    pub fn resolved_settings(&self) -> Result<ResolvedSettings, String> {
        self.stored
            .lock()
            .map_err(|e| e.to_string())?
            .resolve(&config::process_env)
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.lock().map(|s| s.len()).unwrap_or(0)
    }
}

/// 同時に 2 つのサイクルが走らないようにする。
pub struct BusyGuard<'a>(&'a AtomicBool);

impl<'a> BusyGuard<'a> {
    pub fn acquire(flag: &'a AtomicBool) -> Result<Self, String> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "前の処理がまだ終わっていません".to_string())?;
        Ok(Self(flag))
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CycleReport {
    pub explanations: Vec<String>,
    /// 実行した値の変更 (ブロック選択は含めない)。
    pub changes: Vec<DoneAction>,
    /// 途中で止まった理由。最後まで終わったときは None。
    pub error: Option<String>,
    pub recording_peak_dbfs: Option<f32>,
    pub rounds: usize,
    pub undo_depth: usize,
    /// Undo で戻せなかった値 (Undo のときだけ使う)。
    pub not_restored: Vec<RestoreTarget>,
}

impl CycleReport {
    fn empty(recording_peak_dbfs: Option<f32>) -> Self {
        Self {
            explanations: vec![],
            changes: vec![],
            error: None,
            recording_peak_dbfs,
            rounds: 0,
            undo_depth: 0,
            not_restored: vec![],
        }
    }
}

/// サイクルが外の世界に対して行うこと。
#[allow(async_fn_in_trait)]
pub trait Engine {
    fn progress(&self, stage: &str, message: String);
    async fn capture(&mut self) -> Result<Screenshot, String>;
    async fn ask(&mut self, body: &Value) -> Result<AiResponse, String>;
    async fn execute(
        &mut self,
        actions: Vec<AiAction>,
        window: TargetWindow,
    ) -> Result<Outcome, String>;
}

#[derive(Clone, Serialize)]
struct Progress {
    stage: String,
    message: String,
}

fn emit_progress(app: &AppHandle, stage: &str, message: String) {
    let _ = app.emit(
        "progress",
        Progress {
            stage: stage.to_string(),
            message,
        },
    );
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("内部エラー: {e}"))?
}

/// 本物: xcap で撮り、AI API を呼び、enigo で操作する。
struct TauriEngine {
    app: AppHandle,
    settings: ResolvedSettings,
}

impl Engine for TauriEngine {
    fn progress(&self, stage: &str, message: String) {
        emit_progress(&self.app, stage, message);
    }

    async fn capture(&mut self) -> Result<Screenshot, String> {
        let target_app = self.settings.target_app.clone();
        blocking(move || capture::capture(&target_app)).await
    }

    async fn ask(&mut self, body: &Value) -> Result<AiResponse, String> {
        let app = self.app.clone();
        ai::call(&self.settings, body, &move |msg| {
            emit_progress(&app, "ai", msg)
        })
        .await
    }

    async fn execute(
        &mut self,
        actions: Vec<AiAction>,
        window: TargetWindow,
    ) -> Result<Outcome, String> {
        blocking(move || {
            Ok(automation::execute(
                &mut macos::MacDesktop::default(),
                &actions,
                &window,
            ))
        })
        .await
    }
}

/// 処理を始める前に権限を確かめる。弾き終わってから失敗させないため、録音の前に見る。
/// マイクが未確認 (not_determined) のときは録音を始めると macOS が許可を求めるので通す。
fn ensure_permissions(need_microphone: bool) -> Result<(), String> {
    let s = macos::permission_status();
    let mut missing = Vec::new();
    if need_microphone && s.microphone == "denied" {
        missing.push("マイク");
    }
    if s.screen_recording != "granted" {
        missing.push("画面収録");
    }
    if s.accessibility != "granted" {
        missing.push("アクセシビリティ");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} の権限がありません。右上の権限表示からシステム設定で許可してください",
            missing.join("・")
        ))
    }
}

/// 1 サイクルの間、撮り直しても変わらない AI への入力。
struct CycleInput<'a> {
    purpose: Purpose<'a>,
    history: &'a [ChatTurn],
    reference: Option<&'a [u8]>,
    recording: Option<&'a [u8]>,
}

/// 撮影 → AI → 検証 → 実行を、ブロック選択が続く限り繰り返す。
/// 途中で止まっても、それまでの結果は `report` に残す。
async fn run_rounds<E: Engine>(
    engine: &mut E,
    model: &str,
    input: &CycleInput<'_>,
    report: &mut CycleReport,
    mut on_done: impl FnMut(&DoneAction),
) -> Result<(), String> {
    let purpose = &input.purpose;
    let mut done_in_cycle: Vec<DoneAction> = Vec::new();
    for round in 1..=MAX_ROUNDS {
        report.rounds = round;
        engine.progress(
            "capture",
            format!("HX Edit を撮影中 ({round}/{MAX_ROUNDS})"),
        );
        let shot = engine.capture().await?;

        engine.progress("ai", format!("AI ({model}) に問い合わせ中"));
        let body = ai::build_request(
            model,
            &RequestInput {
                purpose: purpose.clone(),
                history: input.history,
                reference_audio: input.reference,
                recording_wav: input.recording,
                screenshot_png: &shot.png,
                screenshot_size: (shot.width, shot.height),
                round,
                max_rounds: MAX_ROUNDS,
                done_in_this_cycle: &done_in_cycle,
            },
        );
        let resp = engine.ask(&body).await?;
        if !resp.explanation.trim().is_empty() {
            report
                .explanations
                .push(resp.explanation.trim().to_string());
        }
        ai::validate(&resp, purpose, round == MAX_ROUNDS)?;

        if resp.actions.is_empty() {
            return Ok(());
        }
        engine.progress(
            "operate",
            format!("HX Edit を操作中 ({} 件)", resp.actions.len()),
        );
        let outcome = engine
            .execute(resp.actions.clone(), shot.window.clone())
            .await?;
        for d in &outcome.done {
            if d.kind == ActionKind::SetValue {
                report.changes.push(d.clone());
            }
            on_done(d);
        }
        done_in_cycle.extend(outcome.done);
        if let Some(e) = outcome.error {
            return Err(e);
        }

        let selected_block = resp
            .actions
            .last()
            .is_some_and(|a| a.kind == ActionKind::SelectBlock);
        if !selected_block {
            return Ok(());
        }
    }
    Ok(())
}

/// 同じパラメータを 2 回変えたときは、最初の変更前の値に戻す。
pub fn restore_targets(changes: &[DoneAction]) -> Vec<RestoreTarget> {
    let mut out: Vec<RestoreTarget> = Vec::new();
    for c in changes.iter().filter(|c| c.kind == ActionKind::SetValue) {
        let dup = out
            .iter()
            .any(|t| norm(&t.block) == norm(&c.block) && norm(&t.parameter) == norm(&c.parameter));
        if !dup {
            out.push(RestoreTarget {
                block: c.block.clone(),
                parameter: c.parameter.clone(),
                value: c.previous_value.clone(),
            });
        }
    }
    out
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Undo で実行した変更を、戻す値の一覧から消し込む。値が一致するものの中で、
/// パラメータ名も一致するものを優先する。
pub fn mark_restored(remaining: &mut Vec<RestoreTarget>, done: &DoneAction) {
    if done.kind != ActionKind::SetValue {
        return;
    }
    let by_name = remaining.iter().position(|t| {
        t.value.trim() == done.value.trim() && norm(&t.parameter) == norm(&done.parameter)
    });
    let by_value = || {
        remaining
            .iter()
            .position(|t| t.value.trim() == done.value.trim())
    };
    if let Some(i) = by_name.or_else(by_value) {
        remaining.remove(i);
    }
}

fn summarize(changes: &[DoneAction]) -> String {
    changes
        .iter()
        .map(|c| {
            format!(
                "{} / {}: {} → {}",
                c.block, c.parameter, c.previous_value, c.value
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 録音済みの音を使って調整する。途中で止まっても、実行した分は Undo できるように残す。
pub async fn adjust<E: Engine>(
    engine: &mut E,
    state: &AppState,
    model: &str,
    instruction: &str,
    recording_wav: &[u8],
    recording_peak_dbfs: f32,
) -> Result<CycleReport, String> {
    let reference = state
        .reference
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .map(|r| r.clip.wav.clone());
    let history = state.chat.lock().map_err(|e| e.to_string())?.clone();

    let mut report = CycleReport::empty(Some(recording_peak_dbfs));
    let input = CycleInput {
        purpose: Purpose::Adjust { instruction },
        history: &history,
        reference: reference.as_deref(),
        recording: Some(recording_wav),
    };
    let result = run_rounds(engine, model, &input, &mut report, |_| {}).await;
    report.error = result.err();

    let targets = restore_targets(&report.changes);
    if !targets.is_empty() {
        let mut stack = state.undo_stack.lock().map_err(|e| e.to_string())?;
        stack.push(targets);
        if stack.len() > MAX_UNDO_GROUPS {
            stack.remove(0);
        }
    }
    {
        let mut chat = state.chat.lock().map_err(|e| e.to_string())?;
        chat.push(ChatTurn {
            role: "user".into(),
            text: if instruction.trim().is_empty() {
                "(指示なしで調整を依頼)".into()
            } else {
                instruction.to_string()
            },
        });
        let mut text = report.explanations.join("\n");
        if !report.changes.is_empty() {
            text.push_str("\n実行した変更:\n");
            text.push_str(&summarize(&report.changes));
        }
        if let Some(e) = &report.error {
            text.push_str(&format!("\n(途中で止まった: {e})"));
        }
        chat.push(ChatTurn {
            role: "assistant".into(),
            text,
        });
    }
    report.undo_depth = state.undo_depth();
    Ok(report)
}

/// 直前のサイクルの変更を元に戻す。戻しきれなかった値は、もう一度 Undo できるように残す。
pub async fn revert<E: Engine>(
    engine: &mut E,
    state: &AppState,
    model: &str,
) -> Result<CycleReport, String> {
    let targets = state
        .undo_stack
        .lock()
        .map_err(|e| e.to_string())?
        .last()
        .cloned()
        .ok_or_else(|| "戻せる変更がありません".to_string())?;

    let mut report = CycleReport::empty(None);
    let mut remaining = targets.clone();
    let input = CycleInput {
        purpose: Purpose::Revert { targets: &targets },
        history: &[],
        reference: None,
        recording: None,
    };
    let result = run_rounds(engine, model, &input, &mut report, |d| {
        mark_restored(&mut remaining, d)
    })
    .await;
    report.error = result.err();

    {
        let mut stack = state.undo_stack.lock().map_err(|e| e.to_string())?;
        stack.pop();
        if !remaining.is_empty() {
            stack.push(remaining.clone());
        }
    }
    if !remaining.is_empty() && report.error.is_none() {
        report.error = Some(format!(
            "{} 個の値を戻せませんでした。もう一度 Undo するか、手で戻してください",
            remaining.len()
        ));
    }
    report.not_restored = remaining;
    {
        let mut chat = state.chat.lock().map_err(|e| e.to_string())?;
        chat.push(ChatTurn {
            role: "assistant".into(),
            text: format!("直前の変更を元に戻した:\n{}", summarize(&report.changes)),
        });
    }
    report.undo_depth = state.undo_depth();
    Ok(report)
}

pub async fn run_cycle(
    app: AppHandle,
    state: &AppState,
    instruction: String,
    device_id: Option<String>,
    seconds: u32,
) -> Result<CycleReport, String> {
    let _busy = BusyGuard::acquire(&state.busy)?;
    let settings = state.resolved_settings()?;
    ensure_permissions(true)?;
    // 録音の前に HX Edit があるかを確かめる (弾き終わってから失敗させない)
    let target_app = settings.target_app.clone();
    blocking(move || capture::find_target_window(&target_app)).await?;

    emit_progress(
        &app,
        "record",
        format!("{seconds} 秒録音中… ギターを弾いてください"),
    );
    let recording = blocking(move || {
        crate::sound::prepare_recording(&crate::audio::record(device_id.as_deref(), seconds)?)
    })
    .await?;
    emit_progress(
        &app,
        "record",
        format!(
            "録音完了 ({:.1} 秒, ピーク {:.1} dBFS)",
            recording.seconds, recording.peak_dbfs
        ),
    );

    let model = settings.model.clone();
    let mut engine = TauriEngine {
        app: app.clone(),
        settings,
    };
    let report = adjust(
        &mut engine,
        state,
        &model,
        &instruction,
        &recording.wav,
        recording.peak_dbfs,
    )
    .await;
    emit_progress(&app, "done", "完了".into());
    report
}

pub async fn undo_last(app: AppHandle, state: &AppState) -> Result<CycleReport, String> {
    let _busy = BusyGuard::acquire(&state.busy)?;
    let settings = state.resolved_settings()?;
    ensure_permissions(false)?;
    let model = settings.model.clone();
    let mut engine = TauriEngine {
        app: app.clone(),
        settings,
    };
    let report = revert(&mut engine, state, &model).await;
    emit_progress(&app, "done", "完了".into());
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(kind: ActionKind, block: &str, param: &str, prev: &str, value: &str) -> DoneAction {
        DoneAction {
            kind,
            block: block.into(),
            parameter: param.into(),
            value: value.into(),
            previous_value: prev.into(),
        }
    }

    #[test]
    fn restore_targets_keep_first_previous_value() {
        let changes = vec![
            done(ActionKind::SetValue, "Amp", "Bass", "5.0", "6.0"),
            done(ActionKind::SelectBlock, "Delay", "", "", ""),
            done(ActionKind::SetValue, "Delay", "Mix", "20", "35"),
            done(ActionKind::SetValue, "amp", " bass ", "6.0", "7.0"),
        ];
        let t = restore_targets(&changes);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].value, "5.0");
        assert_eq!(t[1].parameter, "Mix");
        assert_eq!(t[1].value, "20");
    }

    #[test]
    fn mark_restored_prefers_name_match() {
        let mut remaining = vec![
            RestoreTarget {
                block: "Amp".into(),
                parameter: "Bass".into(),
                value: "5.0".into(),
            },
            RestoreTarget {
                block: "Amp".into(),
                parameter: "Treble".into(),
                value: "5.0".into(),
            },
        ];
        mark_restored(
            &mut remaining,
            &done(ActionKind::SetValue, "Amp", "Treble", "4", "5.0"),
        );
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].parameter, "Bass");
        // 名前が違っても値が一致すれば消し込む
        mark_restored(
            &mut remaining,
            &done(ActionKind::SetValue, "Amp", "Low", "4", "5.0"),
        );
        assert!(remaining.is_empty());
    }

    #[test]
    fn busy_guard_rejects_second_acquire() {
        let flag = AtomicBool::new(false);
        let g = BusyGuard::acquire(&flag).unwrap();
        assert!(BusyGuard::acquire(&flag).is_err());
        drop(g);
        assert!(BusyGuard::acquire(&flag).is_ok());
    }

    fn reference_wav(seconds: usize) -> Vec<u8> {
        // 先頭の 1 秒だけ無音、その後は音が鳴っている 8kHz の WAV
        let mut samples = vec![0.0f32; 8_000];
        samples.extend((0..8_000 * (seconds - 1)).map(|i| if i % 16 < 8 { 0.5 } else { -0.5 }));
        crate::sound::encode_wav(&samples, 1, 8_000).unwrap()
    }

    #[test]
    fn reference_starts_at_the_beginning() {
        let r = Reference::new("a.wav".into(), reference_wav(60), AudioFormat::Wav).unwrap();
        assert_eq!(r.clip.start, 0.0);
        assert_eq!(r.clip.seconds, crate::sound::REFERENCE_CLIP_SECONDS);
    }

    #[test]
    fn reference_start_can_be_moved_and_failure_keeps_the_previous_clip() {
        let mut r = Reference::new("a.wav".into(), reference_wav(60), AudioFormat::Wav).unwrap();
        r.set_start(30.0).unwrap();
        assert_eq!(r.clip.start, 30.0);

        let mut quiet = vec![0.5f32; 8_000];
        quiet.extend(vec![0.0f32; 8_000 * 59]);
        let wav = crate::sound::encode_wav(&quiet, 1, 8_000).unwrap();
        let mut r = Reference::new("q.wav".into(), wav, AudioFormat::Wav).unwrap();
        assert!(r.set_start(30.0).is_err());
        assert_eq!(
            r.clip.start, 0.0,
            "無音の区間を選んでも前の区間を使い続ける"
        );
    }

    #[test]
    fn reference_file_size_is_limited() {
        let e = Reference::new(
            "big.wav".into(),
            vec![0; MAX_REFERENCE_BYTES + 1],
            AudioFormat::Wav,
        )
        .err()
        .unwrap();
        assert!(e.contains("大きすぎます"), "{e}");
    }
}
