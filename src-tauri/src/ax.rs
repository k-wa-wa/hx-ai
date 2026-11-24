//! macOS のアクセシビリティで HX Edit の画面を写し取り、部品を押す (`hx_io::Ui` の本物)。
//! 判断は `ax_tree` と `hx_io` にあり、ここは OS とのやり取りだけにする。

// サイクルに組み込むまで (このあとの変更で使う)、使われない警告を許す
#![allow(dead_code)]

use std::ffi::c_void;
use std::path::Path as FsPath;
use std::time::Duration;

use core_foundation::array::CFArray;
use core_foundation::base::{CFRelease, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use objc2_foundation::NSString;

use crate::ax_tree::Node;
use crate::hx_io::Ui;

pub const HX_EDIT_BUNDLE_ID: &str = "com.line6.hxedit";

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: CFTypeRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> i32;
    fn AXUIElementPerformAction(element: CFTypeRef, action: CFStringRef) -> i32;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
}

const KEY_G: u16 = 5;
/// kCGHIDEventTap: キーボードで打ったのと同じ経路。
const HID_EVENT_TAP: u32 = 0;
const FLAG_SHIFT: u64 = 1 << 17;
const FLAG_COMMAND: u64 = 1 << 20;

/// 写し取る深さと、1 つの部品の子の数の上限 (ファイルの一覧のような大きな部分で時間がかからないように)。
const MENU_DEPTH: usize = 4;
const WINDOW_DEPTH: usize = 10;
const MAX_CHILDREN: usize = 200;

fn attr(el: &CFType, name: &str) -> Option<CFType> {
    let name = CFString::new(name);
    let mut v: CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(el.as_CFTypeRef(), name.as_concrete_TypeRef(), &mut v)
    };
    if err != 0 || v.is_null() {
        return None;
    }
    Some(unsafe { CFType::wrap_under_create_rule(v) })
}

fn text(el: &CFType, name: &str) -> String {
    let Some(v) = attr(el, name) else {
        return String::new();
    };
    if let Some(s) = v.downcast::<CFString>() {
        return s.to_string();
    }
    if let Some(n) = v.downcast::<CFNumber>() {
        return n
            .to_i64()
            .map(|i| i.to_string())
            .or_else(|| n.to_f64().map(|f| f.to_string()))
            .unwrap_or_default();
    }
    if let Some(b) = v.downcast::<CFBoolean>() {
        return if bool::from(b) { "1" } else { "0" }.into();
    }
    String::new()
}

fn enabled(el: &CFType) -> bool {
    attr(el, "AXEnabled")
        .and_then(|v| v.downcast::<CFBoolean>())
        .is_none_or(bool::from)
}

fn elements(v: Option<CFType>) -> Vec<CFType> {
    let Some(array) = v.and_then(|v| v.downcast_into::<CFArray>()) else {
        return Vec::new();
    };
    array
        .get_all_values()
        .into_iter()
        .filter(|p| !p.is_null())
        .map(|p| unsafe { CFType::wrap_under_get_rule(p) })
        .collect()
}

fn children(el: &CFType) -> Vec<CFType> {
    elements(attr(el, "AXChildren"))
}

fn node(el: &CFType, depth: usize) -> Node {
    Node {
        role: text(el, "AXRole"),
        subrole: text(el, "AXSubrole"),
        title: text(el, "AXTitle"),
        id: text(el, "AXIdentifier"),
        desc: text(el, "AXDescription"),
        value: text(el, "AXValue"),
        enabled: enabled(el),
        children: if depth == 0 {
            Vec::new()
        } else {
            children(el)
                .iter()
                .take(MAX_CHILDREN)
                .map(|c| node(c, depth - 1))
                .collect()
        },
    }
}

/// 本物の HX Edit。
pub struct MacUi {
    pid: i32,
    app: CFType,
    last: Option<Node>,
}

impl MacUi {
    /// 起動中の HX Edit につなぐ。
    pub fn connect() -> Result<Self, String> {
        if !unsafe { AXIsProcessTrusted() } {
            return Err("アクセシビリティの許可がありません".into());
        }
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(HX_EDIT_BUNDLE_ID),
        );
        let app = apps.firstObject().ok_or("HX Edit が起動していません")?;
        let pid = app.processIdentifier();
        let el = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid)) };
        unsafe { AXUIElementSetMessagingTimeout(el.as_CFTypeRef(), 2.0) };
        Ok(Self {
            pid,
            app: el,
            last: None,
        })
    }

    /// 根の子: [メニューバー, ウィンドウ...]。`snapshot` と `resolve` で同じ並びにする。
    fn top_level(&self) -> Vec<(CFType, usize)> {
        let mut out = Vec::new();
        if let Some(bar) = attr(&self.app, "AXMenuBar") {
            out.push((bar, MENU_DEPTH));
        }
        for w in elements(attr(&self.app, "AXWindows")) {
            out.push((w, WINDOW_DEPTH));
        }
        out
    }

    /// 直前に写し取った `path` の部品を探し、写し取ったときと同じものかを確かめる。
    fn resolve(&self, path: &[usize]) -> Result<CFType, String> {
        let last = self
            .last
            .as_ref()
            .ok_or("画面を写し取る前に操作しようとしました")?;
        let expected = last
            .get(path)
            .ok_or("写し取った画面に無い部品を操作しようとしました")?;
        let (first, rest) = path.split_first().ok_or("アプリそのものは操作しません")?;
        let mut el = self
            .top_level()
            .into_iter()
            .nth(*first)
            .map(|(e, _)| e)
            .ok_or("画面が変わったので操作しません")?;
        for i in rest {
            el = children(&el)
                .into_iter()
                .nth(*i)
                .ok_or("画面が変わったので操作しません")?;
        }
        let now = node(&el, 0);
        if (&now.role, &now.id, &now.title) != (&expected.role, &expected.id, &expected.title) {
            return Err(format!(
                "画面が変わったので操作しません ({} {} が {} {} になっている)",
                expected.role, expected.title, now.role, now.title
            ));
        }
        Ok(el)
    }

    /// HX Edit が最前面か (HX Edit 自身の答え)。
    fn frontmost(&self) -> bool {
        attr(&self.app, "AXFrontmost")
            .and_then(|v| v.downcast::<CFBoolean>())
            .is_some_and(bool::from)
    }

    fn perform(&self, path: &[usize], action: &str) -> Result<(), String> {
        let el = self.resolve(path)?;
        let a = CFString::new(action);
        let err = unsafe { AXUIElementPerformAction(el.as_CFTypeRef(), a.as_concrete_TypeRef()) };
        if err != 0 {
            return Err(format!("{action} ができません (AXError {err})"));
        }
        Ok(())
    }
}

impl Ui for MacUi {
    fn snapshot(&mut self) -> Result<Node, String> {
        let root = Node {
            role: "AXApplication".into(),
            title: text(&self.app, "AXTitle"),
            enabled: true,
            children: self
                .top_level()
                .iter()
                .map(|(e, depth)| node(e, *depth))
                .collect(),
            ..Node::default()
        };
        if root.children.is_empty() {
            return Err(
                "HX Edit の画面を読めません (アクセシビリティの許可か、HX Edit が応答しない)"
                    .into(),
            );
        }
        self.last = Some(root.clone());
        Ok(root)
    }

    fn press(&mut self, path: &[usize]) -> Result<(), String> {
        self.perform(path, "AXPress")
    }

    fn set_value(&mut self, path: &[usize], value: &str) -> Result<(), String> {
        let el = self.resolve(path)?;
        let name = CFString::new("AXValue");
        let v = CFString::new(value);
        let err = unsafe {
            AXUIElementSetAttributeValue(
                el.as_CFTypeRef(),
                name.as_concrete_TypeRef(),
                v.as_CFTypeRef(),
            )
        };
        if err != 0 {
            return Err(format!("入力欄に文字を入れられません (AXError {err})"));
        }
        Ok(())
    }

    fn confirm(&mut self, path: &[usize]) -> Result<(), String> {
        self.perform(path, "AXConfirm")
    }

    fn open_go_to_folder(&mut self) -> Result<(), String> {
        // プロセスに直接送る (CGEventPostToPid) と、保存の画面に届かなかった。
        // キーボードと同じ経路で送るので、他のアプリに届かないよう、直前に HX Edit が最前面かを確かめる
        if !self.frontmost() {
            return Err("HX Edit が最前面でないので、キーを送りません".into());
        }
        for down in [true, false] {
            let ev = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), KEY_G, down) };
            if ev.is_null() {
                return Err("キーの入力を作れません".into());
            }
            unsafe {
                CGEventSetFlags(ev, FLAG_COMMAND | FLAG_SHIFT);
                CGEventPost(HID_EVENT_TAP, ev);
                CFRelease(ev as CFTypeRef);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        Ok(())
    }

    fn bring_to_front(&mut self) -> Result<(), String> {
        // 最前面かは HX Edit 自身に聞く (AXFrontmost)。NSWorkspace の「最前面のアプリ」は
        // 画面を持たない処理 (cargo test) では更新されず、古いまま (Code など) になった
        if self.frontmost() {
            return Ok(());
        }
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(self.pid)
            .ok_or("HX Edit が終了しています")?;
        #[allow(deprecated)]
        app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            if self.frontmost() {
                return Ok(());
            }
        }
        Err("HX Edit を最前面にできませんでした (HX Edit が最前面でないと答えた)".into())
    }

    fn open_in_hx_edit(&mut self, file: &FsPath) -> Result<(), String> {
        let status = std::process::Command::new("/usr/bin/open")
            .arg("-b")
            .arg(HX_EDIT_BUNDLE_ID)
            .arg(file)
            .status()
            .map_err(|e| format!("HX Edit でファイルを開けません: {e}"))?;
        if !status.success() {
            return Err(format!(
                "HX Edit でファイルを開けません (open の終了コード {status})"
            ));
        }
        Ok(())
    }

    fn file_exists(&self, file: &FsPath) -> bool {
        file.exists()
    }

    fn wait(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}
