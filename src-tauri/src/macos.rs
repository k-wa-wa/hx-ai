//! macOS 固有の処理: マイク権限の確認と要求。

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

use crate::permission::{self, PermissionStatus};

fn microphone_status() -> String {
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return "denied".to_string();
    };
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
    match status {
        AVAuthorizationStatus::Authorized => "granted",
        AVAuthorizationStatus::NotDetermined => "not_determined",
        _ => "denied",
    }
    .to_string()
}

pub fn permission_status() -> PermissionStatus {
    PermissionStatus {
        microphone: microphone_status(),
    }
}

/// 「許可する」を押したときの処理。何をするかは `permission::grant_plan` で決める。
pub fn grant_permission(kind: &str) -> Result<(), String> {
    let status = permission_status();
    let state = match kind {
        "microphone" => &status.microphone,
        other => return Err(format!("不明な権限です: {other}")),
    };
    let plan = permission::grant_plan(kind, state)?;
    if plan.ask {
        ask_permission(kind)?;
    }
    if plan.open_settings {
        open_settings(kind)?;
    }
    Ok(())
}

/// 許可を求める。初めてならダイアログが出る。
fn ask_permission(kind: &str) -> Result<(), String> {
    match kind {
        "microphone" => {
            let media_type = unsafe { AVMediaTypeAudio }
                .ok_or_else(|| "AVMediaTypeAudio が取得できません".to_string())?;
            let handler = RcBlock::new(|_granted: Bool| {});
            unsafe {
                AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &handler)
            };
            Ok(())
        }
        other => Err(format!("不明な権限です: {other}")),
    }
}

fn open_settings(kind: &str) -> Result<(), String> {
    let url = permission::settings_url(kind)?;
    let status = std::process::Command::new("/usr/bin/open")
        .arg(url)
        .status()
        .map_err(|e| format!("システム設定を開けません: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "システム設定を開けません (open の終了コード {status})"
        ))
    }
}
