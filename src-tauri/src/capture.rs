//! HX Edit のウィンドウを探し、そのウィンドウだけをキャプチャする。

use std::io::Cursor;

use serde::Serialize;
use xcap::Window;

use crate::geometry::{MAX_SCREENSHOT_SIDE, Rect, ScreenWindow, fit_within};

#[derive(Debug, Clone, Serialize)]
pub struct TargetWindow {
    pub window_id: u32,
    pub pid: u32,
    pub app_name: String,
    pub title: String,
    /// 画面座標 (ポイント)。
    pub rect: Rect,
}

pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 撮った時点のウィンドウの矩形。AI の座標はこれを基準に変換する。
    pub window: TargetWindow,
}

fn rect_of(w: &Window) -> Option<Rect> {
    Some(Rect {
        x: w.x().ok()? as f64,
        y: w.y().ok()? as f64,
        width: w.width().ok()? as f64,
        height: w.height().ok()? as f64,
    })
}

fn all_windows() -> Result<Vec<Window>, String> {
    Window::all().map_err(|e| format!("ウィンドウの一覧を取得できません: {e}"))
}

/// 指定したアプリ名のウィンドウのうち、最小化されていない一番大きいものを返す。
pub fn find_target_window(app_name: &str) -> Result<TargetWindow, String> {
    let wanted = app_name.trim().to_lowercase();
    let mut best: Option<TargetWindow> = None;
    for w in all_windows()? {
        let Ok(name) = w.app_name() else { continue };
        if name.trim().to_lowercase() != wanted {
            continue;
        }
        if w.is_minimized().unwrap_or(false) {
            continue;
        }
        let (Ok(id), Ok(pid), Some(rect)) = (w.id(), w.pid(), rect_of(&w)) else {
            continue;
        };
        // メニューやツールチップなどの小さな補助ウィンドウは除く
        if rect.width < 200.0 || rect.height < 150.0 {
            continue;
        }
        let candidate = TargetWindow {
            window_id: id,
            pid,
            app_name: name,
            title: w.title().unwrap_or_default(),
            rect,
        };
        let area = rect.width * rect.height;
        if best
            .as_ref()
            .is_none_or(|b| area > b.rect.width * b.rect.height)
        {
            best = Some(candidate);
        }
    }
    best.ok_or_else(|| {
        format!(
            "「{app_name}」のウィンドウが見つかりません。HX Edit を起動し、画面に表示してください"
        )
    })
}

/// ウィンドウの今の矩形。閉じられていれば None。
pub fn current_rect(window_id: u32) -> Result<Option<Rect>, String> {
    Ok(all_windows()?
        .iter()
        .find(|w| w.id().ok() == Some(window_id))
        .and_then(rect_of))
}

/// 画面に出ているウィンドウを手前から奥の順に返す。
pub fn screen_windows() -> Result<Vec<ScreenWindow>, String> {
    let mut out = Vec::new();
    for w in all_windows()? {
        let (Ok(id), Ok(pid), Some(rect)) = (w.id(), w.pid(), rect_of(&w)) else {
            continue;
        };
        out.push(ScreenWindow {
            window_id: id,
            pid,
            owner: w.app_name().unwrap_or_default(),
            rect,
        });
    }
    Ok(out)
}

/// ウィンドウをそのままの解像度で撮る (縮めない)。ダブルクリックの前後を比べるのに使う。
pub fn capture_image(window_id: u32) -> Result<image::RgbaImage, String> {
    let window = all_windows()?
        .into_iter()
        .find(|w| w.id().ok() == Some(window_id))
        .ok_or_else(|| "撮ろうとしたウィンドウが見つかりません".to_string())?;
    window.capture_image().map_err(|e| {
        format!("ウィンドウをキャプチャできません (画面収録の権限を確かめてください): {e}")
    })
}

pub fn capture(app_name: &str) -> Result<Screenshot, String> {
    let target = find_target_window(app_name)?;
    let window = all_windows()?
        .into_iter()
        .find(|w| w.id().ok() == Some(target.window_id))
        .ok_or_else(|| "キャプチャ直前に HX Edit のウィンドウが消えました".to_string())?;
    let image = window.capture_image().map_err(|e| {
        format!(
            "HX Edit のウィンドウをキャプチャできません (画面収録の権限を確かめてください): {e}"
        )
    })?;
    if image.width() == 0 || image.height() == 0 {
        return Err("キャプチャした画像が空です".to_string());
    }
    let (width, height) = fit_within(image.width(), image.height(), MAX_SCREENSHOT_SIDE);
    let image = if (width, height) == image.dimensions() {
        image
    } else {
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle)
    };
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("PNG に変換できません: {e}"))?;
    Ok(Screenshot {
        png,
        width,
        height,
        window: target,
    })
}
