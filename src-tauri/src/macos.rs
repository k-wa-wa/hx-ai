//! macOS 固有の処理: 権限 (マイク・画面収録・アクセシビリティ)。

use std::ffi::c_void;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

use crate::permission::{self, PermissionStatus};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

fn bool_status(granted: bool) -> String {
    if granted { "granted" } else { "denied" }.to_string()
}

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
        screen_recording: bool_status(unsafe { CGPreflightScreenCaptureAccess() }),
        accessibility: bool_status(unsafe { AXIsProcessTrusted() }),
    }
}

/// 「許可する」を押したときの処理。何をするかは `permission::grant_plan` で決める。
pub fn grant_permission(kind: &str) -> Result<(), String> {
    let status = permission_status();
    let state = match kind {
        "microphone" => &status.microphone,
        "screen_recording" => &status.screen_recording,
        "accessibility" => &status.accessibility,
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
        "screen_recording" => {
            unsafe { CGRequestScreenCaptureAccess() };
            Ok(())
        }
        "accessibility" => {
            let key: &NSString = unsafe { &*(kAXTrustedCheckOptionPrompt as *const NSString) };
            let value = NSNumber::new_bool(true);
            let options: Retained<NSDictionary<NSString, NSNumber>> =
                NSDictionary::from_slices(&[key], &[&*value]);
            unsafe { AXIsProcessTrustedWithOptions(Retained::as_ptr(&options) as *const c_void) };
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
