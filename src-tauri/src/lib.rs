mod ai;
#[cfg(test)]
mod ai_http_test;
#[cfg(test)]
mod ai_live_test;
mod audio;
mod automation;
mod capture;
mod config;
mod geometry;
mod macos;
mod sound;
mod workflow;
#[cfg(test)]
mod workflow_test;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::ai::AudioFormat;
use crate::config::{SettingsView, StoredSettings};
use crate::workflow::{AppState, CycleReport, Reference};

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
        .view(&config::process_env))
}

#[tauri::command]
fn save_settings(state: State<AppState>, input: SettingsInput) -> Result<SettingsView, String> {
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
    Ok(stored.view(&config::process_env))
}

#[tauri::command]
async fn list_input_devices() -> Result<Vec<audio::InputDevice>, String> {
    tauri::async_runtime::spawn_blocking(audio::list_input_devices)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn permission_status() -> macos::PermissionStatus {
    macos::permission_status()
}

#[tauri::command]
fn request_permission(kind: String) -> Result<(), String> {
    macos::request_permission(&kind)
}

#[tauri::command]
fn open_permission_settings(kind: String) -> Result<(), String> {
    macos::open_settings(&kind)
}

#[tauri::command]
async fn find_target_window(state: State<'_, AppState>) -> Result<capture::TargetWindow, String> {
    let settings = state.stored.lock().map_err(|e| e.to_string())?.clone();
    let app_name = settings.view(&config::process_env).effective_target_app;
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
    Ok(())
}

#[tauri::command]
async fn run_cycle(
    app: AppHandle,
    state: State<'_, AppState>,
    instruction: String,
    device_id: Option<String>,
    seconds: u32,
) -> Result<CycleReport, String> {
    workflow::run_cycle(app, &state, instruction, device_id, seconds).await
}

#[tauri::command]
async fn undo_last(app: AppHandle, state: State<'_, AppState>) -> Result<CycleReport, String> {
    workflow::undo_last(app, &state).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 開発時はリポジトリ直下の .env を読む。無くてもよい (設定画面で入力できる)
    let _ = dotenvy::dotenv();

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let settings_path = config::settings_path(&config_dir);
            let stored = config::load(&settings_path)?;
            app.manage(AppState::new(settings_path, stored));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            list_input_devices,
            permission_status,
            request_permission,
            open_permission_settings,
            find_target_window,
            set_reference,
            set_reference_start,
            get_reference_audio,
            clear_reference,
            undo_depth,
            clear_conversation,
            run_cycle,
            undo_last,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
