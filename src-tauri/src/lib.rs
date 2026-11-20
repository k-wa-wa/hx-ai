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
use crate::workflow::{AppState, CycleReport, MAX_REFERENCE_BYTES, Reference};

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
fn set_reference(state: State<AppState>, path: String) -> Result<ReferenceInfo, String> {
    let format = AudioFormat::from_path(&path)?;
    let bytes =
        std::fs::read(&path).map_err(|e| format!("音源ファイルを読めません ({path}): {e}"))?;
    if bytes.len() > MAX_REFERENCE_BYTES {
        return Err(format!(
            "音源ファイルが大きすぎます ({:.1} MB)。{} MB 以下に切り出してください",
            bytes.len() as f64 / 1024.0 / 1024.0,
            MAX_REFERENCE_BYTES / 1024 / 1024
        ));
    }
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or(path.clone());
    let info = ReferenceInfo {
        name,
        mime: format.mime().to_string(),
        size: bytes.len(),
    };
    *state.reference.lock().map_err(|e| e.to_string())? = Some(Reference { bytes, format });
    Ok(info)
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
