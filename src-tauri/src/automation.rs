//! AI が返した操作を HX Edit に対して実行する。
//!
//! 操作の前に毎回ガードレールを確かめ、1 つでも外れたらそこで止める。
//! - 最前面のアプリが HX Edit である
//! - スクショを撮った時からウィンドウが動いていない
//! - 座標がウィンドウ内 (タイトルバーと縁を除く) にある
//! - その座標で一番手前のウィンドウが HX Edit である
//! - 前の操作からユーザーがマウスを動かしていない

use std::time::Duration;

use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};

use crate::ai::{ActionKind, AiAction, DoneAction};
use crate::capture::{self, TargetWindow};
use crate::geometry::{self, Point};
use crate::macos;

const AFTER_ACTIVATE: Duration = Duration::from_millis(300);
const AFTER_MOVE: Duration = Duration::from_millis(80);
const BETWEEN_CLICKS: Duration = Duration::from_millis(60);
const AFTER_DOUBLE_CLICK: Duration = Duration::from_millis(200);
const AFTER_TYPING: Duration = Duration::from_millis(80);
const AFTER_ENTER: Duration = Duration::from_millis(250);
const AFTER_SELECT_BLOCK: Duration = Duration::from_millis(500);

/// 途中で止まったときも、それまでに実行した操作を返す (Undo に使う)。
pub struct Outcome {
    pub done: Vec<DoneAction>,
    pub error: Option<String>,
}

fn to_done(a: &AiAction) -> DoneAction {
    DoneAction {
        kind: a.kind,
        block: a.block.clone(),
        parameter: a.parameter.clone(),
        value: a.value.trim().to_string(),
        previous_value: a.previous_value.trim().to_string(),
    }
}

fn round_point(p: Point) -> (i32, i32) {
    (p.x.round() as i32, p.y.round() as i32)
}

/// 操作を実行する前の検証。座標をすべて画面座標に変換し、ウィンドウ内に収まるかを見る。
pub fn plan_points(actions: &[AiAction], window: &TargetWindow) -> Result<Vec<Point>, String> {
    actions
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let p = geometry::normalized_to_screen(window.rect, a.x, a.y)
                .map_err(|e| format!("操作 {} ({} / {}): {e}", i + 1, a.block, a.parameter))?;
            geometry::ensure_inside_window(window.rect, p)
                .map_err(|e| format!("操作 {} ({} / {}): {e}", i + 1, a.block, a.parameter))?;
            Ok(p)
        })
        .collect()
}

fn check_guards(window: &TargetWindow, p: Point) -> Result<(), String> {
    let pid = window.pid as i32;
    macos::ensure_frontmost(pid)?;
    let current = capture::current_rect(window.window_id)?
        .ok_or_else(|| "HX Edit のウィンドウが閉じられました".to_string())?;
    geometry::ensure_same_geometry(window.rect, current)?;
    geometry::ensure_inside_window(current, p)?;
    geometry::ensure_topmost_at(&capture::screen_windows()?, window.window_id, p)?;
    Ok(())
}

/// 検証済みの操作を順に実行する。
pub fn execute(actions: &[AiAction], window: &TargetWindow) -> Outcome {
    let mut done = Vec::new();
    match execute_inner(actions, window, &mut done) {
        Ok(()) => Outcome { done, error: None },
        Err(e) => Outcome {
            done,
            error: Some(e),
        },
    }
}

fn execute_inner(
    actions: &[AiAction],
    window: &TargetWindow,
    done: &mut Vec<DoneAction>,
) -> Result<(), String> {
    if actions.is_empty() {
        return Ok(());
    }
    if macos::permission_status().accessibility != "granted" {
        return Err(
            "アクセシビリティの権限が無いので自動操作できません。システム設定で許可してください"
                .to_string(),
        );
    }
    let points = plan_points(actions, window)?;

    macos::activate(window.pid as i32)?;
    std::thread::sleep(AFTER_ACTIVATE);

    let mut enigo = Enigo::new(&Settings {
        open_prompt_to_get_permissions: false,
        ..Settings::default()
    })
    .map_err(|e| format!("自動操作を初期化できません: {e}"))?;

    let mut last_mouse: Option<(i32, i32)> = None;
    for (a, p) in actions.iter().zip(points) {
        if let Some(expected) = last_mouse {
            let actual = enigo
                .location()
                .map_err(|e| format!("マウスの位置を取得できません: {e}"))?;
            geometry::ensure_mouse_untouched(expected, actual)?;
        }
        check_guards(window, p)?;

        let (x, y) = round_point(p);
        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| format!("マウスを動かせません: {e}"))?;
        std::thread::sleep(AFTER_MOVE);

        match a.kind {
            ActionKind::SetValue => {
                click(&mut enigo)?;
                std::thread::sleep(BETWEEN_CLICKS);
                click(&mut enigo)?;
                std::thread::sleep(AFTER_DOUBLE_CLICK);
                // ダブルクリックで別のウィンドウが開いていないかを確かめてから打ち込む
                macos::ensure_frontmost(window.pid as i32)?;
                enigo
                    .text(a.value.trim())
                    .map_err(|e| format!("値を入力できません: {e}"))?;
                std::thread::sleep(AFTER_TYPING);
                enigo
                    .key(Key::Return, Direction::Click)
                    .map_err(|e| format!("Enter を押せません: {e}"))?;
                std::thread::sleep(AFTER_ENTER);
            }
            ActionKind::SelectBlock => {
                click(&mut enigo)?;
                std::thread::sleep(AFTER_SELECT_BLOCK);
            }
        }
        done.push(to_done(a));
        last_mouse = Some((x, y));
    }
    Ok(())
}

fn click(enigo: &mut Enigo) -> Result<(), String> {
    enigo
        .button(Button::Left, Direction::Click)
        .map_err(|e| format!("クリックできません: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn window() -> TargetWindow {
        TargetWindow {
            window_id: 1,
            pid: 2,
            app_name: "HX Edit".into(),
            title: "HX Edit".into(),
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 1000.0,
            },
        }
    }

    fn action(x: f64, y: f64) -> AiAction {
        AiAction {
            kind: ActionKind::SetValue,
            block: "Amp".into(),
            parameter: "Bass".into(),
            x,
            y,
            value: "5".into(),
            previous_value: "4".into(),
        }
    }

    #[test]
    fn plan_points_converts_all_actions() {
        let pts = plan_points(&[action(500.0, 500.0), action(100.0, 900.0)], &window()).unwrap();
        assert_eq!(pts[0], Point { x: 500.0, y: 500.0 });
        assert_eq!(pts[1], Point { x: 100.0, y: 900.0 });
    }

    #[test]
    fn plan_points_rejects_title_bar_before_doing_anything() {
        let err = plan_points(&[action(500.0, 500.0), action(500.0, 10.0)], &window()).unwrap_err();
        assert!(err.starts_with("操作 2"), "{err}");
    }
}
