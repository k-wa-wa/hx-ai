mod ai;
#[cfg(test)]
mod ai_http_test;
#[cfg(test)]
mod ai_live_test;
mod audio;
mod automation;
mod capture;
mod catalog;
mod config;
mod geometry;
mod hx_device;
mod macos;
mod permission;
mod preset;
mod screen_check;
mod sound;
mod value_fields;
mod workflow;
#[cfg(test)]
mod workflow_test;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::ai::AudioFormat;
use crate::config::{SettingsView, StoredSettings};
use crate::workflow::{AppState, CycleReport, LoopSummary, RecordingInfo, Reference};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInput {
    api_base_url: String,
    /// None なら保存済みのキーを変えない。空文字なら消す。
    api_key: Option<String>,
    model: String,
    target_app: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReferenceInfo {
    name: String,
    mime: String,
    size: usize,
    /// 音源全体の長さ (秒)。
    duration: f32,
    /// AI に送る区間の開始位置と長さ (秒)。
    clip_start: f32,
    clip_seconds: f32,
}

impl From<&Reference> for ReferenceInfo {
    fn from(r: &Reference) -> Self {
        Self {
            name: r.name.clone(),
            mime: r.format.mime().to_string(),
            size: r.bytes.len(),
            duration: r.track.duration(),
            clip_start: r.clip.start,
            clip_seconds: r.clip.seconds,
        }
    }
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Result<SettingsView, String> {
    Ok(state
        .stored
        .lock()
        .map_err(|e| e.to_string())?
        .view(&config::app_env(config::USE_ENV)))
}

/// 設定ウィンドウを開く。開いていれば前に出す。
#[tauri::command]
async fn open_settings_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("settings") {
        w.show().map_err(|e| e.to_string())?;
        return w.set_focus().map_err(|e| e.to_string());
    }
    tauri::WebviewWindowBuilder::new(
        &app,
        "settings",
        tauri::WebviewUrl::App("settings.html".into()),
    )
    .title("HX AI 設定")
    .inner_size(380.0, 600.0)
    .min_inner_size(320.0, 400.0)
    .build()
    .map_err(|e| format!("設定ウィンドウを開けません: {e}"))?;
    Ok(())
}

/// 設定を保存し、メイン画面に "settings_changed" で知らせる。
#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: State<AppState>,
    input: SettingsInput,
) -> Result<SettingsView, String> {
    let mut stored = state.stored.lock().map_err(|e| e.to_string())?;
    let next = StoredSettings {
        api_base_url: input.api_base_url.trim().to_string(),
        api_key: match input.api_key {
            Some(k) => k.trim().to_string(),
            None => stored.api_key.clone(),
        },
        model: input.model.trim().to_string(),
        target_app: input.target_app.trim().to_string(),
    };
    config::save(&state.settings_path, &next)?;
    *stored = next;
    let view = stored.view(&config::app_env(config::USE_ENV));
    let _ = app.emit("settings_changed", &view);
    Ok(view)
}

#[tauri::command]
async fn list_input_devices() -> Result<Vec<audio::InputDevice>, String> {
    tauri::async_runtime::spawn_blocking(audio::list_input_devices)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn permission_status() -> permission::PermissionStatus {
    macos::permission_status()
}

#[tauri::command]
fn grant_permission(kind: String) -> Result<(), String> {
    macos::grant_permission(&kind)
}

#[tauri::command]
async fn find_target_window(state: State<'_, AppState>) -> Result<capture::TargetWindow, String> {
    let settings = state.stored.lock().map_err(|e| e.to_string())?.clone();
    let app_name = settings
        .view(&config::app_env(config::USE_ENV))
        .effective_target_app;
    tauri::async_runtime::spawn_blocking(move || capture::find_target_window(&app_name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_reference(state: State<'_, AppState>, path: String) -> Result<ReferenceInfo, String> {
    let format = AudioFormat::from_path(&path)?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or(path.clone());
    let reference = tauri::async_runtime::spawn_blocking(move || {
        let bytes =
            std::fs::read(&path).map_err(|e| format!("音源ファイルを読めません ({path}): {e}"))?;
        Reference::new(name, bytes, format)
    })
    .await
    .map_err(|e| e.to_string())??;
    let info = ReferenceInfo::from(&reference);
    *state.reference.lock().map_err(|e| e.to_string())? = Some(reference);
    Ok(info)
}

/// AI に送るリファレンスの区間の開始位置 (秒) を変える。
#[tauri::command]
fn set_reference_start(state: State<AppState>, seconds: f32) -> Result<ReferenceInfo, String> {
    let mut guard = state.reference.lock().map_err(|e| e.to_string())?;
    let r = guard
        .as_mut()
        .ok_or_else(|| "リファレンス音源がセットされていません".to_string())?;
    r.set_start(seconds)?;
    Ok(ReferenceInfo::from(&*r))
}

/// プレビュー再生用に、セットしたリファレンス音源のバイト列を返す。
#[tauri::command]
fn get_reference_audio(state: State<AppState>) -> Result<tauri::ipc::Response, String> {
    let guard = state.reference.lock().map_err(|e| e.to_string())?;
    let r = guard
        .as_ref()
        .ok_or_else(|| "リファレンス音源がセットされていません".to_string())?;
    Ok(tauri::ipc::Response::new(r.bytes.clone()))
}

#[tauri::command]
fn clear_reference(state: State<AppState>) -> Result<(), String> {
    *state.reference.lock().map_err(|e| e.to_string())? = None;
    Ok(())
}

#[tauri::command]
fn undo_depth(state: State<AppState>) -> usize {
    state.undo_depth()
}

#[tauri::command]
fn clear_conversation(state: State<AppState>) -> Result<(), String> {
    state.chat.lock().map_err(|e| e.to_string())?.clear();
    state.changed.lock().map_err(|e| e.to_string())?.clear();
    Ok(())
}

#[tauri::command]
fn start_recording(state: State<AppState>, device_id: Option<String>) -> Result<(), String> {
    workflow::start_recording(&state, device_id)
}

#[tauri::command]
async fn stop_recording(state: State<'_, AppState>) -> Result<RecordingInfo, String> {
    workflow::stop_recording(&state).await
}

#[tauri::command]
fn discard_recording(state: State<AppState>) -> Result<(), String> {
    *state.pending.lock().map_err(|e| e.to_string())? = None;
    Ok(())
}

#[tauri::command]
async fn send_message(
    app: AppHandle,
    state: State<'_, AppState>,
    instruction: String,
) -> Result<CycleReport, String> {
    workflow::send_message(app, &state, instruction).await
}

#[tauri::command]
async fn start_loop(
    app: AppHandle,
    state: State<'_, AppState>,
    instruction: String,
    device_id: Option<String>,
    seconds: u32,
    max_iterations: usize,
) -> Result<LoopSummary, String> {
    workflow::run_loop(app, &state, instruction, device_id, seconds, max_iterations).await
}

#[tauri::command]
fn stop_loop(state: State<AppState>) {
    state
        .loop_stop
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

#[tauri::command]
fn add_loop_instruction(state: State<AppState>, text: String) -> Result<(), String> {
    workflow::add_loop_instruction(
        &mut *state.loop_extra.lock().map_err(|e| e.to_string())?,
        &text,
    )
}

#[tauri::command]
async fn undo_last(app: AppHandle, state: State<'_, AppState>) -> Result<CycleReport, String> {
    workflow::undo_last(app, &state).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 開発時はリポジトリ直下の .env を読む。無くてもよい (設定画面で入力できる)
    if config::USE_ENV {
        let _ = dotenvy::dotenv();
    }

    tauri::Builder::default()
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let settings_path = config::settings_path(&config_dir);
            let stored = config::load(&settings_path)?;
            app.manage(AppState::new(settings_path, stored));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            open_settings_window,
            save_settings,
            list_input_devices,
            permission_status,
            grant_permission,
            find_target_window,
            set_reference,
            set_reference_start,
            get_reference_audio,
            clear_reference,
            undo_depth,
            clear_conversation,
            start_recording,
            stop_recording,
            discard_recording,
            send_message,
            start_loop,
            stop_loop,
            add_loop_instruction,
            undo_last,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
