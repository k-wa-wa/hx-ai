//! macOS 固有の処理: 権限 (マイク・画面収録・アクセシビリティ) と、アプリの最前面化。

use std::ffi::c_void;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use serde::Serialize;

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

/// "granted" | "denied" | "not_determined"
#[derive(Debug, Clone, Serialize)]
pub struct PermissionStatus {
    pub microphone: String,
    pub screen_recording: String,
    pub accessibility: String,
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

/// 権限を求めるダイアログを出す。既に拒否されている場合はダイアログが出ないので、
/// 呼び出し側でシステム設定を開く。
pub fn request_permission(kind: &str) -> Result<(), String> {
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

/// システム設定の該当ページの URL。
pub fn settings_url(kind: &str) -> Result<&'static str, String> {
    match kind {
        "microphone" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        }
        "screen_recording" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        }
        "accessibility" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
        other => Err(format!("不明な権限です: {other}")),
    }
}

pub fn open_settings(kind: &str) -> Result<(), String> {
    let url = settings_url(kind)?;
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

/// 最前面のアプリのプロセス ID。
pub fn frontmost_pid() -> Option<i32> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| app.processIdentifier())
}

pub fn frontmost_name() -> String {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|app| app.localizedName())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "不明".to_string())
}

fn wait_frontmost(pid: i32, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if frontmost_pid() == Some(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    frontmost_pid() == Some(pid)
}

/// 指定したプロセスのアプリを最前面にする。最前面にならなければエラーにする。
pub fn activate(pid: i32) -> Result<(), String> {
    if frontmost_pid() == Some(pid) {
        return Ok(());
    }
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .ok_or_else(|| format!("プロセス {pid} のアプリが見つかりません"))?;
    #[allow(deprecated)]
    app.activateWithOptions(
        NSApplicationActivationOptions::ActivateAllWindows
            | NSApplicationActivationOptions::ActivateIgnoringOtherApps,
    );
    if wait_frontmost(pid, Duration::from_millis(1500)) {
        return Ok(());
    }
    // 自アプリが前面にいないと (グローバルショートカット経由など) 上の方法は断られることがある。
    // LaunchServices 経由で開き直す。起動済みのアプリは前面に出るだけである。
    if let Some(path) = app.bundleURL().and_then(|u| u.path()) {
        let _ = std::process::Command::new("/usr/bin/open")
            .arg(path.to_string())
            .status();
        if wait_frontmost(pid, Duration::from_millis(2000)) {
            return Ok(());
        }
    }
    Err(format!(
        "HX Edit を最前面にできませんでした (最前面: {})",
        frontmost_name()
    ))
}

/// 現在の最前面が指定したプロセスであることを確かめる。
pub fn ensure_frontmost(pid: i32) -> Result<(), String> {
    if frontmost_pid() == Some(pid) {
        Ok(())
    } else {
        Err(format!(
            "最前面のアプリが HX Edit ではありません ({})。操作を中断しました",
            frontmost_name()
        ))
    }
}
