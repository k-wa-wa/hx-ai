//! 1 サイクル (チェーン取得 → AI → USB 書き込み) と Undo、自動ループの流れ。
//!
//! 録音・USB 通信・AI は `Engine` トレイトの向こうに置き、テストでは偽物に差し替える。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::ai::{self, AiResponse, AudioFormat, ChatTurn, DoneAction, RequestInput, RestoreTarget};
use crate::audio;
use crate::config::{self, ResolvedSettings, StoredSettings};
use crate::hx_device::{
    self, AppliedChange, ApplyOutcome, ChainSnapshot, RestoreFailure, UsbHxPort,
};
use crate::macos;
use crate::sound::{self, MAX_RECORD_SECONDS, Prepared, ReferenceClip, ReferenceTrack};

/// リファレンス音源ファイルの上限。AI には切り出した区間しか送らないので、
/// 上限は読み込みとプレビューの重さで決める。数分の WAV (数十 MB) は受け付ける。
pub const MAX_REFERENCE_BYTES: usize = 50 * 1024 * 1024;
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
    /// Undo 用。1 要素が 1 サイクル分の適用記録。
    pub undo_stack: Mutex<Vec<Vec<AppliedChange>>>,
    pub chat: Mutex<Vec<ChatTurn>>,
    /// このセッションでアプリが変えた値 (パラメータごとに最後の値)。会話の履歴は
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
    /// HX Stomp との USB 接続 (1 セッションを保持して再接続待ちを省く)。
    pub hx: Arc<Mutex<Option<UsbHxPort>>>,
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
            hx: Arc::new(Mutex::new(None)),
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

    #[allow(dead_code)]
    pub fn disconnect_hx(&self) {
        if let Ok(mut guard) = self.hx.lock()
            && let Some(port) = guard.as_mut()
        {
            port.disconnect();
        }
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
            rounds: 1,
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
    async fn read_chain(&mut self) -> Result<ChainSnapshot, String>;
    async fn ask(&mut self, body: &Value) -> Result<AiResponse, String>;
    async fn apply(
        &mut self,
        snap: &ChainSnapshot,
        changes: &[hx_device::Change],
    ) -> Result<ApplyOutcome, String>;
    async fn restore(&mut self, applied: &[AppliedChange]) -> Vec<RestoreFailure>;
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

fn with_usb<T>(
    hx: &Mutex<Option<UsbHxPort>>,
    f: impl FnOnce(&mut UsbHxPort) -> Result<T, String>,
) -> Result<T, String> {
    let mut guard = hx.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        *guard = Some(UsbHxPort::new()?);
    }
    let port = guard.as_mut().expect("UsbHxPort initialized");
    f(port)
}

/// 本物: cpal で録り、hx_device で USB 読み書きし、AI API を呼ぶ。
struct TauriEngine {
    app: AppHandle,
    settings: ResolvedSettings,
    hx: Arc<Mutex<Option<UsbHxPort>>>,
    /// 自動ループで録音するデバイスと秒数。
    device_id: Option<String>,
    seconds: u32,
}

impl Engine for TauriEngine {
    fn progress(&self, stage: &str, message: String) {
        emit_progress(&self.app, stage, message);
    }

    async fn read_chain(&mut self) -> Result<ChainSnapshot, String> {
        let hx = self.hx.clone();
        blocking(move || with_usb(&hx, hx_device::HxPort::read_snapshot)).await
    }

    async fn ask(&mut self, body: &Value) -> Result<AiResponse, String> {
        let app = self.app.clone();
        ai::call(&self.settings, body, &move |msg| {
            emit_progress(&app, "ai", msg)
        })
        .await
    }

    async fn apply(
        &mut self,
        snap: &ChainSnapshot,
        changes: &[hx_device::Change],
    ) -> Result<ApplyOutcome, String> {
        let hx = self.hx.clone();
        let snap = snap.clone();
        let changes = changes.to_vec();
        blocking(move || with_usb(&hx, |p| hx_device::apply_changes(p, &snap, &changes))).await
    }

    async fn restore(&mut self, applied: &[AppliedChange]) -> Vec<RestoreFailure> {
        let hx = self.hx.clone();
        let to_restore = applied.to_vec();
        blocking(move || with_usb(&hx, |p| Ok(hx_device::restore_changes(p, &to_restore))))
            .await
            .unwrap_or_else(|reason| {
                applied
                    .iter()
                    .rev()
                    .map(|a| RestoreFailure {
                        block: a.block_id.clone(),
                        parameter: a.param_name.clone(),
                        value: a.previous_display.clone(),
                        reason: reason.clone(),
                    })
                    .collect()
            })
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

/// マイクの権限だけ確かめる (USB 直接方式では画面収録とアクセシビリティは不要)。
fn ensure_microphone_permission() -> Result<(), String> {
    if macos::permission_status().microphone == "granted" {
        return Ok(());
    }
    macos::grant_permission("microphone")
        .map_err(|e| format!("マイク の権限が必要ですが、許可を求められませんでした: {e}"))?;
    Err("マイク の権限が必要です。許可してから、もう一度実行してください".into())
}

fn done_from_applied(a: &AppliedChange) -> DoneAction {
    DoneAction {
        block: a.block_id.clone(),
        parameter: a.param_name.clone(),
        value: a.new_display.clone(),
        previous_value: a.previous_display.clone(),
    }
}

/// 実行した値の変更を「変えたパラメータ」の一覧に反映する。同じパラメータは最後の値だけを
/// 一覧の末尾に残し、上限を超えたら古いものから消す。
pub fn record_changes(log: &mut Vec<RestoreTarget>, changes: &[DoneAction]) {
    for c in changes {
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
    let mut applied_for_undo: Vec<AppliedChange> = Vec::new();

    let result: Result<(), String> = async {
        engine.progress("capture", "HX Stomp から現在の設定を取得中".into());
        let snap = engine.read_chain().await?;
        let chain_text = hx_device::render_chain(&snap);

        engine.progress("ai", format!("AI ({model}) に問い合わせ中"));
        let body = ai::build_request(
            model,
            &RequestInput {
                instruction,
                history: &history,
                reference_audio: reference.as_deref(),
                recording_wav: req.recording.map(|r| r.wav.as_slice()),
                chain_text: &chain_text,
                changed_parameters: &changed,
            },
        );
        let resp = engine.ask(&body).await?;
        if !resp.explanation.trim().is_empty() {
            report
                .explanations
                .push(resp.explanation.trim().to_string());
        }
        ai::validate(&resp)?;
        report.goal_reached = resp.goal_reached;

        if resp.changes.is_empty() {
            return Ok(());
        }
        if req.stop.is_some_and(|s| s.load(Ordering::SeqCst)) {
            return Err(STOPPED_BEFORE_EXECUTE.to_string());
        }

        engine.progress(
            "operate",
            format!("HX Stomp に反映中 ({} 件)", resp.changes.len()),
        );
        let device_changes: Vec<hx_device::Change> = resp
            .changes
            .iter()
            .map(|c| hx_device::Change {
                block: c.block.clone(),
                parameter: c.parameter.clone(),
                value: c.value.clone(),
            })
            .collect();
        let outcome = engine.apply(&snap, &device_changes).await?;
        for a in &outcome.applied {
            report.changes.push(done_from_applied(a));
        }
        applied_for_undo = outcome.applied;
        if let Some(e) = outcome.error {
            return Err(e);
        }
        Ok(())
    }
    .await;

    report.error = result.err();

    record_changes(
        &mut *state.changed.lock().map_err(|e| e.to_string())?,
        &report.changes,
    );
    if !applied_for_undo.is_empty() {
        let mut stack = state.undo_stack.lock().map_err(|e| e.to_string())?;
        stack.push(applied_for_undo);
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

/// 直前のサイクルの変更を元に戻す (AI は呼ばず、記録した元の生値を直接書き戻す)。
/// 戻しきれなかった値は、もう一度 Undo できるように残す。
pub async fn revert<E: Engine>(engine: &mut E, state: &AppState) -> Result<CycleReport, String> {
    let applied = state
        .undo_stack
        .lock()
        .map_err(|e| e.to_string())?
        .last()
        .cloned()
        .ok_or_else(|| "戻せる変更がありません".to_string())?;

    let mut report = CycleReport::empty(None);
    engine.progress(
        "operate",
        format!("HX Stomp の変更を元に戻しています ({} 件)", applied.len()),
    );
    let failures = engine.restore(&applied).await;
    let failed_keys: HashSet<(usize, usize)> = failures
        .iter()
        .filter_map(|f| {
            applied
                .iter()
                .find(|a| a.block_id == f.block && a.param_name == f.parameter)
                .map(|a| (a.slot, a.param_index))
        })
        .collect();

    let mut remaining = Vec::new();
    for a in applied {
        if failed_keys.contains(&(a.slot, a.param_index)) {
            remaining.push(a);
        } else {
            report.changes.push(DoneAction {
                block: a.block_id,
                parameter: a.param_name,
                value: a.previous_display,
                previous_value: a.new_display,
            });
        }
    }

    record_changes(
        &mut *state.changed.lock().map_err(|e| e.to_string())?,
        &report.changes,
    );

    let not_restored: Vec<RestoreTarget> = failures
        .iter()
        .map(|f| RestoreTarget {
            block: f.block.clone(),
            parameter: f.parameter.clone(),
            value: f.value.clone(),
        })
        .collect();

    {
        let mut stack = state.undo_stack.lock().map_err(|e| e.to_string())?;
        stack.pop();
        if !remaining.is_empty() {
            stack.push(remaining);
        }
    }
    if !not_restored.is_empty() {
        report.error = Some(format!(
            "{} 個の値を戻せませんでした ({})。もう一度 Undo するか、手で戻してください",
            not_restored.len(),
            failures[0].reason
        ));
    }
    report.not_restored = not_restored;
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
/// 何も変えずに失敗したとき (AI の混雑や USB エラーなど) は、送り直せるように残す。
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
/// - `MAX_CONSECUTIVE_FAILURES` 回続けて失敗した (無音・AI の混雑・USB 通信エラーなど)
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
        macos::grant_permission("microphone")?;
        return Err("マイクの権限が必要です。許可してから、もう一度録音してください".to_string());
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
    let model = settings.model.clone();
    let mut engine = TauriEngine {
        app: app.clone(),
        settings,
        hx: state.hx.clone(),
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
    ensure_microphone_permission()?;
    let model = settings.model.clone();
    let mut engine = TauriEngine {
        app: app.clone(),
        settings,
        hx: state.hx.clone(),
        device_id,
        seconds,
    };
    // 録音の前に HX Stomp が USB で繋がっているか確かめる
    engine.read_chain().await?;
    let summary = auto_loop(&mut engine, state, &model, &instruction, max_iterations).await;
    emit_progress(&app, "done", "完了".into());
    summary
}

pub async fn undo_last(app: AppHandle, state: &AppState) -> Result<CycleReport, String> {
    let _busy = BusyGuard::acquire(&state.busy)?;
    let settings = state.resolved_settings()?;
    let mut engine = TauriEngine {
        app: app.clone(),
        settings,
        hx: state.hx.clone(),
        device_id: None,
        seconds: 0,
    };
    let report = revert(&mut engine, state).await;
    emit_progress(&app, "done", "完了".into());
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(block: &str, param: &str, prev: &str, value: &str) -> DoneAction {
        DoneAction {
            block: block.into(),
            parameter: param.into(),
            value: value.into(),
            previous_value: prev.into(),
        }
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
                done("block5", "Bass", "5", "6"),
                done("block5", "Treble", "7", "6.5"),
            ],
        );
        record_changes(&mut log, &[done("BLOCK5", " bass", "6", "7")]);
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
            .map(|i| done("block5", &format!("P{i}"), "0", "1"))
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
