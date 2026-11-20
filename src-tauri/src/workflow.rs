//! 1 サイクル (キャプチャ → AI → 自動操作) と Undo、自動ループの流れ。
//!
//! 録音・キャプチャ・AI・自動操作は `Engine` トレイトの向こうに置き、テストでは偽物に差し替える。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::ai::{
    self, ActionKind, AiAction, AiResponse, AudioFormat, ChatTurn, DoneAction, Purpose,
    RequestInput, RestoreTarget,
};
use crate::audio;
use crate::automation::{self, Outcome};
use crate::capture::{self, Screenshot, TargetWindow};
use crate::config::{self, ResolvedSettings, StoredSettings};
use crate::macos;
use crate::sound::{self, MAX_RECORD_SECONDS, Prepared, ReferenceClip, ReferenceTrack};

/// 1 サイクルで撮り直して AI に聞き直す回数の上限。
pub const MAX_ROUNDS: usize = 4;

/// リファレンス音源ファイルの上限。AI には切り出した区間しか送らないので、
/// 上限は読み込みとプレビューの重さで決める。数分の WAV (数十 MB) は受け付ける。
pub const MAX_REFERENCE_BYTES: usize = 50 * 1024 * 1024;
// 数分の WAV (44.1kHz ステレオで 1 分 約 10MB) を受け付けること
const _: () = assert!(MAX_REFERENCE_BYTES >= 50 * 1024 * 1024);

/// Undo 履歴に残す数。
pub const MAX_UNDO_GROUPS: usize = 20;

/// AI に伝える「このセッションで変えたパラメータ」の数。古いものから忘れる。
pub const MAX_CHANGED_PARAMETERS: usize = 24;

/// 自動ループで、変えた値が音に反映されるのを待ってから次の録音を始める。
pub const LOOP_SETTLE: Duration = Duration::from_secs(1);
/// 自動ループで、続けてこの回数失敗したら止める。
pub const MAX_CONSECUTIVE_FAILURES: usize = 3;
/// 自動ループの回数の上限として選べる最大値。
pub const MAX_LOOP_ITERATIONS: usize = 50;
/// 自動ループ中に追加された指示を覚えておく数。古いものから忘れる。
pub const MAX_LOOP_EXTRA_INSTRUCTIONS: usize = 5;

/// 停止の合図を受けたため、AI の提案を実行せずに止めたときのエラー。
pub const STOPPED_BEFORE_EXECUTE: &str = "停止の合図を受けたので、AI の提案は実行していません";

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
    /// このセッションでアプリが打ち込んだ値 (パラメータごとに最後の値)。会話の履歴は
    /// 切り詰めて送るので、どのパラメータをどこまで動かしたかはこちらで伝える。
    pub changed: Mutex<Vec<RestoreTarget>>,
    pub busy: AtomicBool,
    /// 手動モードで録音中の入力。
    pub recorder: Mutex<Option<audio::Recorder>>,
    /// 手動モードで録り終えて、まだ送っていない録音。
    pub pending: Mutex<Option<Prepared>>,
    /// 自動ループの停止の合図。
    pub loop_stop: Arc<AtomicBool>,
    /// 自動ループ中に追加された指示。
    pub loop_extra: Arc<Mutex<Vec<String>>>,
}

impl AppState {
    pub fn new(settings_path: std::path::PathBuf, stored: StoredSettings) -> Self {
        Self {
            settings_path,
            stored: Mutex::new(stored),
            reference: Mutex::new(None),
            undo_stack: Mutex::new(Vec::new()),
            chat: Mutex::new(Vec::new()),
            changed: Mutex::new(Vec::new()),
            busy: AtomicBool::new(false),
            recorder: Mutex::new(None),
            pending: Mutex::new(None),
            loop_stop: Arc::new(AtomicBool::new(false)),
            loop_extra: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn resolved_settings(&self) -> Result<ResolvedSettings, String> {
        self.stored
            .lock()
            .map_err(|e| e.to_string())?
            .resolve(&config::app_env(config::USE_ENV))
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
    /// AI が「目標に届いた。これ以上変えなくてよい」と判断したか。
    pub goal_reached: bool,
    /// 自動操作がガードレール (最前面・マウスの介入など) で止まったか。
    pub interrupted: bool,
    /// 送った録音が使われずに残っているか (手動モードで、何も変えずに失敗したとき)。
    pub recording_kept: bool,
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
            goal_reached: false,
            interrupted: false,
            recording_kept: false,
        }
    }
}

/// 録り終えた録音の情報 (画面に出す)。
#[derive(Debug, Clone, Serialize)]
pub struct RecordingInfo {
    pub seconds: f32,
    pub peak_dbfs: f32,
}

impl From<&Prepared> for RecordingInfo {
    fn from(p: &Prepared) -> Self {
        Self {
            seconds: p.seconds,
            peak_dbfs: p.peak_dbfs,
        }
    }
}

/// 自動ループが終わったときの結果。
#[derive(Debug, Clone, Serialize)]
pub struct LoopSummary {
    pub iterations: usize,
    /// 止まった理由。
    pub reason: String,
    pub undo_depth: usize,
}

/// 自動ループの 1 回分の結果 (画面に送るイベント)。
#[derive(Debug, Clone, Serialize)]
pub struct LoopIteration {
    pub iteration: usize,
    pub max: usize,
    pub report: CycleReport,
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
    /// 自動ループ用に録音する。
    async fn record(&mut self) -> Result<Prepared, String>;
    /// 自動ループの 1 回分の結果を画面に伝える。
    fn iteration(&mut self, report: LoopIteration);
    /// 変えた値が音に反映されるのを待つ。
    async fn settle(&mut self);
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

/// 本物: cpal で録り、xcap で撮り、AI API を呼び、enigo で操作する。
struct TauriEngine {
    app: AppHandle,
    settings: ResolvedSettings,
    /// 自動ループで録音するデバイスと秒数。
    device_id: Option<String>,
    seconds: u32,
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

    async fn record(&mut self) -> Result<Prepared, String> {
        let device_id = self.device_id.clone();
        let seconds = self.seconds;
        blocking(move || sound::prepare_recording(&audio::record(device_id.as_deref(), seconds)?))
            .await
    }

    fn iteration(&mut self, report: LoopIteration) {
        let _ = self.app.emit("loop_iteration", report);
    }

    async fn settle(&mut self) {
        tokio::time::sleep(LOOP_SETTLE).await;
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
            "{} の権限がありません。右上の ⚙︎ (設定) の「macOS の権限」から許可してください",
            missing.join("・")
        ))
    }
}

/// 1 サイクルの間、撮り直しても変わらない AI への入力。
struct CycleInput<'a> {
    purpose: Purpose<'a>,
    history: &'a [ChatTurn],
    changed: &'a [RestoreTarget],
    reference: Option<&'a [u8]>,
    recording: Option<&'a [u8]>,
    /// 立っていたら、AI の提案を実行する前に止める。
    stop: Option<&'a AtomicBool>,
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
                earlier_explanations: &report.explanations,
                changed_parameters: input.changed,
            },
        );
        let resp = engine.ask(&body).await?;
        if !resp.explanation.trim().is_empty() {
            report
                .explanations
                .push(resp.explanation.trim().to_string());
        }
        ai::validate(&resp, purpose, round == MAX_ROUNDS)?;
        report.goal_reached = resp.goal_reached;

        if resp.actions.is_empty() {
            return Ok(());
        }
        if input.stop.is_some_and(|s| s.load(Ordering::SeqCst)) {
            return Err(STOPPED_BEFORE_EXECUTE.to_string());
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
            report.interrupted = true;
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

/// 実行した値の変更を「変えたパラメータ」の一覧に反映する。同じパラメータは最後の値だけを
/// 一覧の末尾に残し、上限を超えたら古いものから消す。
pub fn record_changes(log: &mut Vec<RestoreTarget>, changes: &[DoneAction]) {
    for c in changes.iter().filter(|c| c.kind == ActionKind::SetValue) {
        log.retain(|t| {
            !(norm(&t.block) == norm(&c.block) && norm(&t.parameter) == norm(&c.parameter))
        });
        log.push(RestoreTarget {
            block: c.block.clone(),
            parameter: c.parameter.clone(),
            value: c.value.clone(),
        });
    }
    let excess = log.len().saturating_sub(MAX_CHANGED_PARAMETERS);
    log.drain(..excess);
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

/// 調整 1 回分の入力。
pub struct AdjustInput<'a> {
    pub instruction: &'a str,
    pub recording: Option<&'a Prepared>,
    /// 会話の履歴に残すユーザーの発言。None なら指示をそのまま残す。
    pub history_label: Option<&'a str>,
    /// 立っていたら、AI の提案を実行する前に止める。
    pub stop: Option<&'a AtomicBool>,
}

/// 指示と録音 (あれば) を使って調整する。途中で止まっても、実行した分は Undo できるように残す。
pub async fn adjust<E: Engine>(
    engine: &mut E,
    state: &AppState,
    model: &str,
    req: &AdjustInput<'_>,
) -> Result<CycleReport, String> {
    let instruction = req.instruction;
    let reference = state
        .reference
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .map(|r| r.clip.wav.clone());
    let history = state.chat.lock().map_err(|e| e.to_string())?.clone();
    let changed = state.changed.lock().map_err(|e| e.to_string())?.clone();

    let mut report = CycleReport::empty(req.recording.map(|r| r.peak_dbfs));
    let input = CycleInput {
        purpose: Purpose::Adjust { instruction },
        history: &history,
        changed: &changed,
        reference: reference.as_deref(),
        recording: req.recording.map(|r| r.wav.as_slice()),
        stop: req.stop,
    };
    let result = run_rounds(engine, model, &input, &mut report, |_| {}).await;
    report.error = result.err();

    record_changes(
        &mut *state.changed.lock().map_err(|e| e.to_string())?,
        &report.changes,
    );
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
            text: match req.history_label {
                Some(label) => label.to_string(),
                None if instruction.trim().is_empty() => "(指示なしで調整を依頼)".into(),
                None => instruction.to_string(),
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
        changed: &[],
        reference: None,
        recording: None,
        stop: None,
    };
    let result = run_rounds(engine, model, &input, &mut report, |d| {
        mark_restored(&mut remaining, d)
    })
    .await;
    report.error = result.err();
    record_changes(
        &mut *state.changed.lock().map_err(|e| e.to_string())?,
        &report.changes,
    );

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

/// 手動モード: 指示と録音 (あれば) を送る。録音は、値を変えたか最後まで終わったら使い切る。
/// 何も変えずに失敗したとき (AI の混雑など) は、送り直せるように残す。
pub async fn send<E: Engine>(
    engine: &mut E,
    state: &AppState,
    model: &str,
    instruction: &str,
) -> Result<CycleReport, String> {
    let recording = state.pending.lock().map_err(|e| e.to_string())?.clone();
    if instruction.trim().is_empty() && recording.is_none() {
        return Err("指示を書くか、録音してから送ってください".to_string());
    }
    let mut report = adjust(
        engine,
        state,
        model,
        &AdjustInput {
            instruction,
            recording: recording.as_ref(),
            history_label: None,
            stop: None,
        },
    )
    .await?;
    if recording.is_some() {
        if report.error.is_none() || !report.changes.is_empty() {
            *state.pending.lock().map_err(|e| e.to_string())? = None;
        } else {
            report.recording_kept = true;
        }
    }
    Ok(report)
}

/// 自動ループの指示。途中で追加された指示を後ろに足す。
pub fn loop_instruction(base: &str, extras: &[String]) -> String {
    let mut text = base.trim().to_string();
    if !extras.is_empty() {
        text.push_str("\n途中で追加された指示 (後のものほど優先する):");
        for e in extras {
            text.push_str(&format!("\n- {e}"));
        }
    }
    text
}

/// 自動ループ中に指示を足す。次の回から使う。
pub fn add_loop_instruction(extras: &mut Vec<String>, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("空の指示は追加できません".to_string());
    }
    extras.push(text.to_string());
    let excess = extras.len().saturating_sub(MAX_LOOP_EXTRA_INSTRUCTIONS);
    extras.drain(..excess);
    Ok(())
}

pub fn check_iterations(max_iterations: usize) -> Result<(), String> {
    if (1..=MAX_LOOP_ITERATIONS).contains(&max_iterations) {
        Ok(())
    } else {
        Err(format!(
            "ループの回数は 1〜{MAX_LOOP_ITERATIONS} 回にしてください: {max_iterations}"
        ))
    }
}

/// 自動ループ: 録音 → 調整 を繰り返す。次のどれかで止まる。
/// - 停止の合図 (`state.loop_stop`)
/// - 回数の上限
/// - AI が値を変えなかった (目標に届いた / 自動で変えられる値が無い)
/// - 自動操作がガードレールで止まった (ユーザーがマウスに触ったなど)
/// - `MAX_CONSECUTIVE_FAILURES` 回続けて失敗した (無音・AI の混雑など)
pub async fn auto_loop<E: Engine>(
    engine: &mut E,
    state: &AppState,
    model: &str,
    instruction: &str,
    max_iterations: usize,
) -> Result<LoopSummary, String> {
    check_iterations(max_iterations)?;
    let stop = state.loop_stop.clone();
    stop.store(false, Ordering::SeqCst);
    state.loop_extra.lock().map_err(|e| e.to_string())?.clear();
    let stopped = || stop.load(Ordering::SeqCst);
    const STOPPED: &str = "停止ボタンで止めた";

    let mut iterations = 0;
    let mut failures = 0;
    let reason = loop {
        if iterations >= max_iterations {
            break format!("上限の {max_iterations} 回に達した");
        }
        if stopped() {
            break STOPPED.to_string();
        }
        let n = iterations + 1;
        engine.progress(
            "record",
            format!("ループ {n}/{max_iterations}: 録音中… 音を出し続けてください"),
        );
        let recorded = engine.record().await;
        if stopped() {
            break STOPPED.to_string();
        }
        let report = match recorded {
            Ok(recording) => {
                let extras = state.loop_extra.lock().map_err(|e| e.to_string())?.clone();
                let label = format!("(自動ループ {n} 回目)");
                adjust(
                    engine,
                    state,
                    model,
                    &AdjustInput {
                        instruction: &loop_instruction(instruction, &extras),
                        recording: Some(&recording),
                        history_label: Some(&label),
                        stop: Some(&stop),
                    },
                )
                .await?
            }
            Err(e) => CycleReport {
                error: Some(e),
                undo_depth: state.undo_depth(),
                ..CycleReport::empty(None)
            },
        };
        iterations = n;
        engine.iteration(LoopIteration {
            iteration: n,
            max: max_iterations,
            report: report.clone(),
        });

        if let Some(e) = &report.error {
            if stopped() {
                break STOPPED.to_string();
            }
            if report.interrupted {
                break format!("自動操作が止まったので、ループも止めた: {e}");
            }
            failures += 1;
            if failures >= MAX_CONSECUTIVE_FAILURES {
                break format!("{failures} 回続けて失敗したので止めた: {e}");
            }
            continue;
        }
        failures = 0;
        if report.changes.is_empty() {
            break if report.goal_reached {
                "AI が目標に届いたと判断した".to_string()
            } else {
                "AI が自動で変えられる値を返さなかった".to_string()
            };
        }
        engine.settle().await;
    };
    Ok(LoopSummary {
        iterations,
        reason,
        undo_depth: state.undo_depth(),
    })
}

/// 手動モードの録音を始める。`MAX_RECORD_SECONDS` 秒で自動的に止まる。
pub fn start_recording(state: &AppState, device_id: Option<String>) -> Result<(), String> {
    if state.busy.load(Ordering::SeqCst) {
        return Err("処理中は録音できません".to_string());
    }
    if macos::permission_status().microphone == "denied" {
        return Err(
            "マイクの権限がありません。右上の ⚙︎ (設定) の「macOS の権限」から許可してください"
                .to_string(),
        );
    }
    let mut recorder = state.recorder.lock().map_err(|e| e.to_string())?;
    if recorder.is_some() {
        return Err("すでに録音中です".to_string());
    }
    *state.pending.lock().map_err(|e| e.to_string())? = None;
    *recorder = Some(audio::Recorder::start(
        device_id.as_deref(),
        Duration::from_secs(MAX_RECORD_SECONDS as u64),
    )?);
    Ok(())
}

/// 手動モードの録音を止め、前後の無音を削って送る準備をする。
pub async fn stop_recording(state: &AppState) -> Result<RecordingInfo, String> {
    let recorder = state
        .recorder
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or_else(|| "録音していません".to_string())?;
    let prepared = blocking(move || sound::prepare_recording(&recorder.stop()?)).await?;
    let info = RecordingInfo::from(&prepared);
    *state.pending.lock().map_err(|e| e.to_string())? = Some(prepared);
    Ok(info)
}

pub async fn send_message(
    app: AppHandle,
    state: &AppState,
    instruction: String,
) -> Result<CycleReport, String> {
    let _busy = BusyGuard::acquire(&state.busy)?;
    if state.recorder.lock().map_err(|e| e.to_string())?.is_some() {
        return Err("録音中です。録音を止めてから送ってください".to_string());
    }
    let settings = state.resolved_settings()?;
    ensure_permissions(false)?;
    let model = settings.model.clone();
    let mut engine = TauriEngine {
        app: app.clone(),
        settings,
        device_id: None,
        seconds: 0,
    };
    let report = send(&mut engine, state, &model, &instruction).await;
    emit_progress(&app, "done", "完了".into());
    report
}

pub async fn run_loop(
    app: AppHandle,
    state: &AppState,
    instruction: String,
    device_id: Option<String>,
    seconds: u32,
    max_iterations: usize,
) -> Result<LoopSummary, String> {
    let _busy = BusyGuard::acquire(&state.busy)?;
    sound::check_duration(seconds)?;
    check_iterations(max_iterations)?;
    let settings = state.resolved_settings()?;
    ensure_permissions(true)?;
    // 録音の前に HX Edit があるかを確かめる (弾き終わってから失敗させない)
    let target_app = settings.target_app.clone();
    blocking(move || capture::find_target_window(&target_app)).await?;
    let model = settings.model.clone();
    let mut engine = TauriEngine {
        app: app.clone(),
        settings,
        device_id,
        seconds,
    };
    let summary = auto_loop(&mut engine, state, &model, &instruction, max_iterations).await;
    emit_progress(&app, "done", "完了".into());
    summary
}

pub async fn undo_last(app: AppHandle, state: &AppState) -> Result<CycleReport, String> {
    let _busy = BusyGuard::acquire(&state.busy)?;
    let settings = state.resolved_settings()?;
    ensure_permissions(false)?;
    let model = settings.model.clone();
    let mut engine = TauriEngine {
        app: app.clone(),
        settings,
        device_id: None,
        seconds: 0,
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

    #[test]
    fn record_changes_keeps_last_value_per_parameter_at_the_end() {
        let mut log = vec![];
        record_changes(
            &mut log,
            &[
                done(ActionKind::SetValue, "Amp", "Bass", "5", "6"),
                done(ActionKind::SetValue, "Amp", "Treble", "7", "6.5"),
                done(ActionKind::SelectBlock, "Delay", "", "", ""),
            ],
        );
        record_changes(
            &mut log,
            &[done(ActionKind::SetValue, "amp", " bass", "6", "7")],
        );
        let got: Vec<(&str, &str)> = log
            .iter()
            .map(|t| (t.parameter.as_str(), t.value.as_str()))
            .collect();
        assert_eq!(got, vec![("Treble", "6.5"), (" bass", "7")]);
    }

    #[test]
    fn record_changes_forgets_oldest_beyond_the_limit() {
        let mut log = vec![];
        let changes: Vec<DoneAction> = (0..MAX_CHANGED_PARAMETERS + 2)
            .map(|i| done(ActionKind::SetValue, "Amp", &format!("P{i}"), "0", "1"))
            .collect();
        record_changes(&mut log, &changes);
        assert_eq!(log.len(), MAX_CHANGED_PARAMETERS);
        assert_eq!(log[0].parameter, "P2");
        assert_eq!(MAX_CHANGED_PARAMETERS, 24);
    }

    #[test]
    fn loop_instruction_appends_extras_in_order() {
        assert_eq!(loop_instruction(" 太く ", &[]), "太く");
        let text = loop_instruction("太く", &["もっと".into(), "やっぱり控えめに".into()]);
        assert_eq!(
            text,
            "太く\n途中で追加された指示 (後のものほど優先する):\n- もっと\n- やっぱり控えめに"
        );
    }

    #[test]
    fn add_loop_instruction_rejects_empty_and_forgets_oldest() {
        let mut extras = vec![];
        assert!(add_loop_instruction(&mut extras, "  ").is_err());
        for i in 0..MAX_LOOP_EXTRA_INSTRUCTIONS + 1 {
            add_loop_instruction(&mut extras, &format!(" 指示{i} ")).unwrap();
        }
        assert_eq!(extras.len(), MAX_LOOP_EXTRA_INSTRUCTIONS);
        assert_eq!(extras[0], "指示1");
        assert_eq!(
            extras.last().unwrap(),
            &format!("指示{MAX_LOOP_EXTRA_INSTRUCTIONS}")
        );
    }

    #[test]
    fn iteration_count_range() {
        assert!(check_iterations(0).is_err());
        assert!(check_iterations(1).is_ok());
        assert!(check_iterations(MAX_LOOP_ITERATIONS).is_ok());
        assert!(check_iterations(MAX_LOOP_ITERATIONS + 1).is_err());
    }

    #[test]
    fn loop_constants() {
        assert_eq!(MAX_CONSECUTIVE_FAILURES, 3);
        assert_eq!(LOOP_SETTLE, Duration::from_secs(1));
        assert_eq!(MAX_LOOP_EXTRA_INSTRUCTIONS, 5);
    }
}
