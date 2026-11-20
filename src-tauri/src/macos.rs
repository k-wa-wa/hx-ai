//! macOS 固有の処理: 権限 (マイク・画面収録・アクセシビリティ) と、アプリの最前面化。

use std::ffi::c_void;
use std::time::{Duration, Instant};

use block2::RcBlock;
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

use crate::automation::Desktop;
use crate::capture;
use crate::geometry::{Rect, ScreenWindow};
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

/// 本物の macOS。enigo は最初の入力操作のときに作る (権限が無いうちは作らない)。
#[derive(Default)]
pub struct MacDesktop {
    enigo: Option<Enigo>,
}

impl MacDesktop {
    fn enigo(&mut self) -> Result<&mut Enigo, String> {
        if self.enigo.is_none() {
            let e = Enigo::new(&Settings {
                open_prompt_to_get_permissions: false,
                ..Settings::default()
            })
            .map_err(|e| format!("自動操作を初期化できません: {e}"))?;
            self.enigo = Some(e);
        }
        Ok(self.enigo.as_mut().expect("直前で作った"))
    }
}

impl Desktop for MacDesktop {
    fn accessibility_granted(&self) -> bool {
        permission_status().accessibility == "granted"
    }
    fn activate(&mut self, pid: i32) -> Result<(), String> {
        activate(pid)
    }
    fn frontmost(&self) -> (Option<i32>, String) {
        (frontmost_pid(), frontmost_name())
    }
    fn window_rect(&self, window_id: u32) -> Result<Option<Rect>, String> {
        capture::current_rect(window_id)
    }
    fn screen_windows(&self) -> Result<Vec<ScreenWindow>, String> {
        capture::screen_windows()
    }
    fn mouse_location(&self) -> Result<(i32, i32), String> {
        match &self.enigo {
            Some(e) => e
                .location()
                .map_err(|e| format!("マウスの位置を取得できません: {e}")),
            None => Err("自動操作が初期化されていません".to_string()),
        }
    }
    fn move_mouse(&mut self, x: i32, y: i32) -> Result<(), String> {
        self.enigo()?
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| format!("マウスを動かせません: {e}"))
    }
    fn click(&mut self) -> Result<(), String> {
        self.enigo()?
            .button(Button::Left, Direction::Click)
            .map_err(|e| format!("クリックできません: {e}"))
    }
    fn type_text(&mut self, text: &str) -> Result<(), String> {
        self.enigo()?
            .text(text)
            .map_err(|e| format!("値を入力できません: {e}"))
    }
    fn press_enter(&mut self) -> Result<(), String> {
        self.enigo()?
            .key(Key::Return, Direction::Click)
            .map_err(|e| format!("Enter を押せません: {e}"))
    }
    fn wait(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}
