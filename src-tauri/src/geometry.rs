//! 座標の変換とガードレールの判定。OS に触らない純粋な計算だけを置く。
//!
//! 座標系は 3 つある。
//! - 正規化座標: AI が返す 0〜1000 の値。スクショの左上が (0, 0)、右下が (1000, 1000)。
//! - 画像座標: スクショのピクセル。Retina ではウィンドウのポイント数の 2 倍になる。
//! - 画面座標: macOS のグローバル座標 (ポイント、左上原点)。enigo と CGWindowList が使う。
//!
//! スクショはウィンドウ全体 (タイトルバー込み) を写すので、正規化座標は
//! ウィンドウの矩形にそのまま対応させればよい。

use serde::Serialize;

/// 正規化座標の上限。
pub const NORMALIZED_MAX: f64 = 1000.0;

/// タイトルバーとみなす上端の高さ (ポイント)。ここをダブルクリックするとウィンドウが
/// 拡大・縮小してしまうので、操作の対象にしない。
pub const TITLE_BAR_HEIGHT: f64 = 28.0;

/// ウィンドウの縁からこれより内側だけを操作の対象にする (ポイント)。
pub const EDGE_MARGIN: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.x + self.width && p.y >= self.y && p.y < self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// 正規化座標を画面座標に変換する。範囲外の値はエラーにする (丸めて続けない)。
pub fn normalized_to_screen(window: Rect, nx: f64, ny: f64) -> Result<Point, String> {
    if !nx.is_finite() || !ny.is_finite() {
        return Err(format!("座標が数値ではありません: ({nx}, {ny})"));
    }
    if !(0.0..=NORMALIZED_MAX).contains(&nx) || !(0.0..=NORMALIZED_MAX).contains(&ny) {
        return Err(format!("座標が 0〜1000 の範囲外です: ({nx}, {ny})"));
    }
    Ok(Point {
        x: window.x + nx / NORMALIZED_MAX * window.width,
        y: window.y + ny / NORMALIZED_MAX * window.height,
    })
}

/// 操作してよい範囲 (ウィンドウからタイトルバーと縁を除いた矩形)。
pub fn operable_area(window: Rect) -> Rect {
    Rect {
        x: window.x + EDGE_MARGIN,
        y: window.y + TITLE_BAR_HEIGHT,
        width: (window.width - EDGE_MARGIN * 2.0).max(0.0),
        height: (window.height - TITLE_BAR_HEIGHT - EDGE_MARGIN).max(0.0),
    }
}

/// 対象座標がウィンドウの操作可能範囲に収まっているかを確かめる。
pub fn ensure_inside_window(window: Rect, p: Point) -> Result<(), String> {
    if !window.contains(p) {
        return Err(format!(
            "座標 ({:.0}, {:.0}) が HX Edit のウィンドウ ({:.0}, {:.0}, {:.0}×{:.0}) の外です",
            p.x, p.y, window.x, window.y, window.width, window.height
        ));
    }
    if !operable_area(window).contains(p) {
        return Err(format!(
            "座標 ({:.0}, {:.0}) がタイトルバーかウィンドウの縁にかかっています",
            p.x, p.y
        ));
    }
    Ok(())
}

/// キャプチャした時のウィンドウ位置・大きさから動いていないかを確かめる。
/// 動いていれば AI が出した座標は古いので、操作してはいけない。
pub fn ensure_same_geometry(captured: Rect, current: Rect) -> Result<(), String> {
    const TOLERANCE: f64 = 1.0;
    let same = (captured.x - current.x).abs() <= TOLERANCE
        && (captured.y - current.y).abs() <= TOLERANCE
        && (captured.width - current.width).abs() <= TOLERANCE
        && (captured.height - current.height).abs() <= TOLERANCE;
    if same {
        Ok(())
    } else {
        Err(format!(
            "スクショを撮った後に HX Edit のウィンドウが動きました ({:.0}, {:.0}, {:.0}×{:.0} → {:.0}, {:.0}, {:.0}×{:.0})",
            captured.x,
            captured.y,
            captured.width,
            captured.height,
            current.x,
            current.y,
            current.width,
            current.height
        ))
    }
}

/// 画面に出ているウィンドウ 1 枚分 (手前から奥の順に並べて使う)。
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenWindow {
    pub window_id: u32,
    pub pid: u32,
    pub owner: String,
    pub rect: Rect,
}

/// 対象座標で一番手前にあるウィンドウが、操作したいウィンドウであるかを確かめる。
///
/// `windows` は手前から奥の順に並んでいること。対象のウィンドウより手前に、
/// その点を覆う別プロセスのウィンドウ (常に手前に表示した自アプリや通知など) があれば
/// クリックがそちらに当たるのでエラーにする。
pub fn ensure_topmost_at(
    windows: &[ScreenWindow],
    target_window_id: u32,
    p: Point,
) -> Result<(), String> {
    for w in windows {
        if w.window_id == target_window_id {
            return Ok(());
        }
        if w.rect.contains(p) {
            return Err(format!(
                "座標 ({:.0}, {:.0}) の上に「{}」のウィンドウが重なっています。HX Edit が隠れないように配置してください",
                p.x, p.y, w.owner
            ));
        }
    }
    Err("HX Edit のウィンドウが画面に見つかりません".to_string())
}

/// 自動操作の途中でユーザーがマウスを動かしたかを確かめる。動かしていれば中断する。
pub fn ensure_mouse_untouched(expected: (i32, i32), actual: (i32, i32)) -> Result<(), String> {
    const TOLERANCE: i32 = 3;
    if (expected.0 - actual.0).abs() <= TOLERANCE && (expected.1 - actual.1).abs() <= TOLERANCE {
        Ok(())
    } else {
        Err("自動操作の途中でマウスが動かされたので中断しました".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: Rect = Rect {
        x: 100.0,
        y: 50.0,
        width: 1200.0,
        height: 800.0,
    };

    #[test]
    fn normalized_maps_to_window_rect() {
        let p = normalized_to_screen(WIN, 0.0, 0.0).unwrap();
        assert_eq!(p, Point { x: 100.0, y: 50.0 });
        let p = normalized_to_screen(WIN, 500.0, 500.0).unwrap();
        assert_eq!(p, Point { x: 700.0, y: 450.0 });
        let p = normalized_to_screen(WIN, 1000.0, 1000.0).unwrap();
        assert_eq!(
            p,
            Point {
                x: 1300.0,
                y: 850.0
            }
        );
    }

    #[test]
    fn normalized_out_of_range_is_rejected() {
        assert!(normalized_to_screen(WIN, -1.0, 10.0).is_err());
        assert!(normalized_to_screen(WIN, 10.0, 1000.1).is_err());
        assert!(normalized_to_screen(WIN, f64::NAN, 10.0).is_err());
    }

    #[test]
    fn inside_window_accepts_content_area() {
        assert!(ensure_inside_window(WIN, Point { x: 700.0, y: 600.0 }).is_ok());
    }

    #[test]
    fn inside_window_rejects_outside_and_title_bar() {
        // 外側
        assert!(ensure_inside_window(WIN, Point { x: 99.0, y: 300.0 }).is_err());
        assert!(
            ensure_inside_window(
                WIN,
                Point {
                    x: 1300.0,
                    y: 300.0
                }
            )
            .is_err()
        );
        assert!(ensure_inside_window(WIN, Point { x: 500.0, y: 850.0 }).is_err());
        // タイトルバー
        let err = ensure_inside_window(WIN, Point { x: 500.0, y: 60.0 }).unwrap_err();
        assert!(err.contains("タイトルバー"));
        // 縁
        assert!(ensure_inside_window(WIN, Point { x: 101.0, y: 300.0 }).is_err());
    }

    #[test]
    fn geometry_change_is_detected() {
        assert!(ensure_same_geometry(WIN, WIN).is_ok());
        let moved = Rect { x: 150.0, ..WIN };
        assert!(ensure_same_geometry(WIN, moved).is_err());
        let resized = Rect {
            width: 1000.0,
            ..WIN
        };
        assert!(ensure_same_geometry(WIN, resized).is_err());
    }

    fn sw(id: u32, pid: u32, owner: &str, rect: Rect) -> ScreenWindow {
        ScreenWindow {
            window_id: id,
            pid,
            owner: owner.to_string(),
            rect,
        }
    }

    #[test]
    fn topmost_ok_when_target_is_in_front() {
        let windows = vec![
            sw(
                1,
                10,
                "Menubar",
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 2000.0,
                    height: 25.0,
                },
            ),
            sw(7, 20, "HX Edit", WIN),
            sw(
                3,
                30,
                "Finder",
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 2000.0,
                    height: 2000.0,
                },
            ),
        ];
        assert!(ensure_topmost_at(&windows, 7, Point { x: 700.0, y: 600.0 }).is_ok());
    }

    #[test]
    fn topmost_rejects_overlapping_window() {
        let companion = Rect {
            x: 1100.0,
            y: 100.0,
            width: 360.0,
            height: 720.0,
        };
        let windows = vec![sw(9, 99, "hx-ai", companion), sw(7, 20, "HX Edit", WIN)];
        let err = ensure_topmost_at(
            &windows,
            7,
            Point {
                x: 1200.0,
                y: 600.0,
            },
        )
        .unwrap_err();
        assert!(err.contains("hx-ai"));
        // 重なっていない場所なら通る
        assert!(ensure_topmost_at(&windows, 7, Point { x: 500.0, y: 600.0 }).is_ok());
    }

    #[test]
    fn topmost_rejects_missing_target() {
        let windows = vec![sw(3, 30, "Finder", WIN)];
        assert!(ensure_topmost_at(&windows, 7, Point { x: 500.0, y: 600.0 }).is_err());
    }

    #[test]
    fn mouse_movement_is_detected() {
        assert!(ensure_mouse_untouched((100, 100), (101, 99)).is_ok());
        assert!(ensure_mouse_untouched((100, 100), (140, 100)).is_err());
    }
}
