//! AI が返した操作を HX Edit に対して実行する。
//!
//! 操作の前に毎回ガードレールを確かめ、1 つでも外れたらそこで止める。
//! - 最前面のアプリが HX Edit である
//! - スクショを撮った時からウィンドウが動いていない
//! - 座標がウィンドウ内 (タイトルバーと縁を除く) にある
//! - その座標で一番手前のウィンドウが HX Edit である
//! - 前の操作からユーザーがマウスを動かしていない
//!
//! OS への操作は `Desktop` トレイトの向こうに置き、テストでは偽物に差し替える。

use std::time::Duration;

use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};

use crate::ai::{ActionKind, AiAction, DoneAction};
use crate::capture::{self, TargetWindow};
use crate::geometry::{self, Point, Rect, ScreenWindow};
use crate::macos;

const AFTER_ACTIVATE: Duration = Duration::from_millis(300);
const AFTER_MOVE: Duration = Duration::from_millis(80);
const BETWEEN_CLICKS: Duration = Duration::from_millis(60);
const AFTER_DOUBLE_CLICK: Duration = Duration::from_millis(200);
const AFTER_TYPING: Duration = Duration::from_millis(80);
const AFTER_ENTER: Duration = Duration::from_millis(250);
const AFTER_SELECT_BLOCK: Duration = Duration::from_millis(500);

/// 自動操作に使う OS の機能。
pub trait Desktop {
    fn accessibility_granted(&self) -> bool;
    /// 指定したプロセスを最前面にする。できなければエラー。
    fn activate(&mut self, pid: i32) -> Result<(), String>;
    /// 最前面のアプリのプロセス ID と名前。
    fn frontmost(&self) -> (Option<i32>, String);
    /// ウィンドウの今の矩形。閉じられていれば None。
    fn window_rect(&self, window_id: u32) -> Result<Option<Rect>, String>;
    /// 画面に出ているウィンドウ (手前から奥の順)。
    fn screen_windows(&self) -> Result<Vec<ScreenWindow>, String>;
    fn mouse_location(&self) -> Result<(i32, i32), String>;
    fn move_mouse(&mut self, x: i32, y: i32) -> Result<(), String>;
    fn click(&mut self) -> Result<(), String>;
    fn type_text(&mut self, text: &str) -> Result<(), String>;
    fn press_enter(&mut self) -> Result<(), String>;
    fn wait(&mut self, d: Duration);
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
        macos::permission_status().accessibility == "granted"
    }
    fn activate(&mut self, pid: i32) -> Result<(), String> {
        macos::activate(pid)
    }
    fn frontmost(&self) -> (Option<i32>, String) {
        (macos::frontmost_pid(), macos::frontmost_name())
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

fn ensure_frontmost(desktop: &dyn Desktop, pid: i32) -> Result<(), String> {
    match desktop.frontmost() {
        (Some(p), _) if p == pid => Ok(()),
        (_, name) => Err(format!(
            "最前面のアプリが HX Edit ではありません ({name})。操作を中断しました"
        )),
    }
}

fn check_guards(desktop: &dyn Desktop, window: &TargetWindow, p: Point) -> Result<(), String> {
    ensure_frontmost(desktop, window.pid as i32)?;
    let current = desktop
        .window_rect(window.window_id)?
        .ok_or_else(|| "HX Edit のウィンドウが閉じられました".to_string())?;
    geometry::ensure_same_geometry(window.rect, current)?;
    geometry::ensure_inside_window(current, p)?;
    geometry::ensure_topmost_at(&desktop.screen_windows()?, window.window_id, p)?;
    Ok(())
}

/// 検証済みの操作を順に実行する。
pub fn execute(desktop: &mut dyn Desktop, actions: &[AiAction], window: &TargetWindow) -> Outcome {
    let mut done = Vec::new();
    match execute_inner(desktop, actions, window, &mut done) {
        Ok(()) => Outcome { done, error: None },
        Err(e) => Outcome {
            done,
            error: Some(e),
        },
    }
}

fn execute_inner(
    desktop: &mut dyn Desktop,
    actions: &[AiAction],
    window: &TargetWindow,
    done: &mut Vec<DoneAction>,
) -> Result<(), String> {
    if actions.is_empty() {
        return Ok(());
    }
    if !desktop.accessibility_granted() {
        return Err(
            "アクセシビリティの権限が無いので自動操作できません。システム設定で許可してください"
                .to_string(),
        );
    }
    let points = plan_points(actions, window)?;

    desktop.activate(window.pid as i32)?;
    desktop.wait(AFTER_ACTIVATE);

    let mut last_mouse: Option<(i32, i32)> = None;
    for (a, p) in actions.iter().zip(points) {
        if let Some(expected) = last_mouse {
            geometry::ensure_mouse_untouched(expected, desktop.mouse_location()?)?;
        }
        check_guards(desktop, window, p)?;

        let (x, y) = round_point(p);
        desktop.move_mouse(x, y)?;
        desktop.wait(AFTER_MOVE);

        match a.kind {
            ActionKind::SetValue => {
                desktop.click()?;
                desktop.wait(BETWEEN_CLICKS);
                desktop.click()?;
                desktop.wait(AFTER_DOUBLE_CLICK);
                // ダブルクリックで別のウィンドウが開いていないかを確かめてから打ち込む
                ensure_frontmost(desktop, window.pid as i32)?;
                desktop.type_text(a.value.trim())?;
                desktop.wait(AFTER_TYPING);
                desktop.press_enter()?;
                desktop.wait(AFTER_ENTER);
            }
            ActionKind::SelectBlock => {
                desktop.click()?;
                desktop.wait(AFTER_SELECT_BLOCK);
            }
        }
        done.push(to_done(a));
        last_mouse = Some((x, y));
    }
    Ok(())
}

#[cfg(test)]
pub mod fake {
    //! テスト用の偽のデスクトップ。入力操作を記録し、操作に反応して状態を変えられる。

    use super::*;

    pub const HX_PID: i32 = 2;
    pub const HX_WINDOW: u32 = 1;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Ev {
        Activate(i32),
        Move(i32, i32),
        Click,
        Type(String),
        Enter,
    }

    /// 偽のデスクトップの状態。テストから書き換えて「途中で何かが起きた」を再現する。
    pub struct World {
        pub accessibility: bool,
        pub activate_ok: bool,
        pub frontmost: Option<i32>,
        pub rect: Option<Rect>,
        /// HX Edit より手前にあるウィンドウ。
        pub overlays: Vec<ScreenWindow>,
        pub mouse: (i32, i32),
    }

    type Hook = Box<dyn FnMut(&Ev, &mut World)>;

    pub struct FakeDesktop {
        pub world: World,
        pub events: Vec<Ev>,
        /// 入力操作のたびに呼ばれる。
        pub hook: Option<Hook>,
    }

    pub fn hx_rect() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 1000.0,
        }
    }

    impl FakeDesktop {
        pub fn new() -> Self {
            Self {
                world: World {
                    accessibility: true,
                    activate_ok: true,
                    // 最初は自アプリが前面にいる
                    frontmost: Some(99),
                    rect: Some(hx_rect()),
                    overlays: vec![],
                    mouse: (0, 0),
                },
                events: vec![],
                hook: None,
            }
        }

        pub fn on_event(mut self, f: impl FnMut(&Ev, &mut World) + 'static) -> Self {
            self.hook = Some(Box::new(f));
            self
        }

        fn record(&mut self, ev: Ev) {
            if let Some(h) = self.hook.as_mut() {
                h(&ev, &mut self.world);
            }
            self.events.push(ev);
        }

        pub fn input_events(&self) -> Vec<Ev> {
            self.events
                .iter()
                .filter(|e| !matches!(e, Ev::Activate(_)))
                .cloned()
                .collect()
        }
    }

    impl Desktop for FakeDesktop {
        fn accessibility_granted(&self) -> bool {
            self.world.accessibility
        }
        fn activate(&mut self, pid: i32) -> Result<(), String> {
            if !self.world.activate_ok {
                return Err("最前面にできない".into());
            }
            self.world.frontmost = Some(pid);
            self.record(Ev::Activate(pid));
            Ok(())
        }
        fn frontmost(&self) -> (Option<i32>, String) {
            (self.world.frontmost, "Other".into())
        }
        fn window_rect(&self, window_id: u32) -> Result<Option<Rect>, String> {
            assert_eq!(window_id, HX_WINDOW);
            Ok(self.world.rect)
        }
        fn screen_windows(&self) -> Result<Vec<ScreenWindow>, String> {
            let mut v = self.world.overlays.clone();
            if let Some(rect) = self.world.rect {
                v.push(ScreenWindow {
                    window_id: HX_WINDOW,
                    pid: HX_PID as u32,
                    owner: "HX Edit".into(),
                    rect,
                });
            }
            Ok(v)
        }
        fn mouse_location(&self) -> Result<(i32, i32), String> {
            Ok(self.world.mouse)
        }
        fn move_mouse(&mut self, x: i32, y: i32) -> Result<(), String> {
            self.world.mouse = (x, y);
            self.record(Ev::Move(x, y));
            Ok(())
        }
        fn click(&mut self) -> Result<(), String> {
            self.record(Ev::Click);
            Ok(())
        }
        fn type_text(&mut self, text: &str) -> Result<(), String> {
            self.record(Ev::Type(text.into()));
            Ok(())
        }
        fn press_enter(&mut self) -> Result<(), String> {
            self.record(Ev::Enter);
            Ok(())
        }
        fn wait(&mut self, _d: Duration) {}
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Ev, FakeDesktop, HX_PID, HX_WINDOW, World, hx_rect};
    use super::*;

    fn window() -> TargetWindow {
        TargetWindow {
            window_id: HX_WINDOW,
            pid: HX_PID as u32,
            app_name: "HX Edit".into(),
            title: "HX Edit".into(),
            rect: hx_rect(),
        }
    }

    fn set_value(x: f64, y: f64, value: &str) -> AiAction {
        AiAction {
            kind: ActionKind::SetValue,
            block: "Amp".into(),
            parameter: "Bass".into(),
            x,
            y,
            value: value.into(),
            previous_value: "4".into(),
        }
    }

    fn select_block(x: f64, y: f64) -> AiAction {
        AiAction {
            kind: ActionKind::SelectBlock,
            block: "Delay".into(),
            parameter: String::new(),
            x,
            y,
            value: String::new(),
            previous_value: String::new(),
        }
    }

    /// 1 個目の操作の Enter が押された後に `f` で状態を変える。
    fn after_first_enter(f: impl Fn(&mut World) + 'static) -> FakeDesktop {
        let mut seen = false;
        FakeDesktop::new().on_event(move |ev, w| {
            if *ev == Ev::Enter && !seen {
                seen = true;
                f(w);
            }
        })
    }

    fn set_value_events(x: i32, y: i32, v: &str) -> Vec<Ev> {
        vec![
            Ev::Move(x, y),
            Ev::Click,
            Ev::Click,
            Ev::Type(v.into()),
            Ev::Enter,
        ]
    }

    #[test]
    fn plan_points_converts_all_actions() {
        let pts = plan_points(
            &[set_value(500.0, 500.0, "5"), set_value(100.0, 900.0, "5")],
            &window(),
        )
        .unwrap();
        assert_eq!(pts[0], Point { x: 500.0, y: 500.0 });
        assert_eq!(pts[1], Point { x: 100.0, y: 900.0 });
    }

    #[test]
    fn plan_points_rejects_title_bar_before_doing_anything() {
        let err = plan_points(
            &[set_value(500.0, 500.0, "5"), set_value(500.0, 10.0, "5")],
            &window(),
        )
        .unwrap_err();
        assert!(err.starts_with("操作 2"), "{err}");
    }

    #[test]
    fn set_value_double_clicks_types_and_enters_in_order() {
        let mut d = FakeDesktop::new();
        let out = execute(
            &mut d,
            &[
                set_value(500.0, 500.0, " 6.0 "),
                set_value(300.0, 800.0, "-3"),
            ],
            &window(),
        );
        assert_eq!(out.error, None);
        assert_eq!(out.done.len(), 2);
        assert_eq!(out.done[0].value, "6.0", "前後の空白は落として記録する");
        assert_eq!(
            d.events[0],
            Ev::Activate(HX_PID),
            "最初に HX Edit を前面に出す"
        );
        let mut expected = set_value_events(500, 500, "6.0");
        expected.extend(set_value_events(300, 800, "-3"));
        assert_eq!(d.input_events(), expected);
    }

    #[test]
    fn select_block_is_a_single_click_without_typing() {
        let mut d = FakeDesktop::new();
        let out = execute(&mut d, &[select_block(580.0, 190.0)], &window());
        assert_eq!(out.error, None);
        assert_eq!(d.input_events(), vec![Ev::Move(580, 190), Ev::Click]);
        assert_eq!(out.done[0].kind, ActionKind::SelectBlock);
    }

    #[test]
    fn empty_actions_do_nothing() {
        let mut d = FakeDesktop::new();
        let out = execute(&mut d, &[], &window());
        assert_eq!(out.error, None);
        assert!(d.events.is_empty());
    }

    #[test]
    fn without_accessibility_nothing_happens() {
        let mut d = FakeDesktop::new();
        d.world.accessibility = false;
        let out = execute(&mut d, &[set_value(500.0, 500.0, "5")], &window());
        assert!(out.error.unwrap().contains("アクセシビリティ"));
        assert!(d.events.is_empty());
    }

    #[test]
    fn invalid_point_anywhere_stops_before_activation() {
        let mut d = FakeDesktop::new();
        let out = execute(
            &mut d,
            &[set_value(500.0, 500.0, "5"), set_value(500.0, 5.0, "5")],
            &window(),
        );
        assert!(out.error.is_some());
        assert!(d.events.is_empty(), "1 つでも不正なら HX Edit に触れない");
        assert!(out.done.is_empty());
    }

    #[test]
    fn activation_failure_stops_before_input() {
        let mut d = FakeDesktop::new();
        d.world.activate_ok = false;
        let out = execute(&mut d, &[set_value(500.0, 500.0, "5")], &window());
        assert!(out.error.is_some());
        assert!(d.input_events().is_empty());
    }

    #[test]
    fn stops_when_another_app_comes_to_front() {
        let mut d = after_first_enter(|w| w.frontmost = Some(42));
        let out = execute(
            &mut d,
            &[set_value(500.0, 500.0, "5"), set_value(300.0, 800.0, "6")],
            &window(),
        );
        assert!(out.error.unwrap().contains("最前面"));
        assert_eq!(out.done.len(), 1, "1 つ目は実行済みとして Undo に残す");
        assert_eq!(d.input_events(), set_value_events(500, 500, "5"));
    }

    #[test]
    fn stops_when_window_moves() {
        let mut d = after_first_enter(|w| {
            w.rect = Some(Rect {
                x: 50.0,
                ..hx_rect()
            })
        });
        let out = execute(
            &mut d,
            &[set_value(500.0, 500.0, "5"), set_value(300.0, 800.0, "6")],
            &window(),
        );
        assert!(out.error.unwrap().contains("動きました"));
        assert_eq!(out.done.len(), 1);
    }

    #[test]
    fn stops_when_window_is_closed() {
        let mut d = after_first_enter(|w| w.rect = None);
        let out = execute(
            &mut d,
            &[set_value(500.0, 500.0, "5"), set_value(300.0, 800.0, "6")],
            &window(),
        );
        assert!(out.error.unwrap().contains("閉じられ"));
        assert_eq!(out.done.len(), 1);
    }

    #[test]
    fn stops_when_another_window_covers_the_point() {
        let mut d = FakeDesktop::new();
        d.world.overlays.push(ScreenWindow {
            window_id: 9,
            pid: 99,
            owner: "HX AI".into(),
            rect: Rect {
                x: 200.0,
                y: 700.0,
                width: 200.0,
                height: 200.0,
            },
        });
        let out = execute(
            &mut d,
            &[set_value(500.0, 500.0, "5"), set_value(300.0, 800.0, "6")],
            &window(),
        );
        assert!(out.error.unwrap().contains("HX AI"));
        assert_eq!(
            d.input_events(),
            set_value_events(500, 500, "5"),
            "覆われた 2 つ目の座標にはマウスを動かさない"
        );
    }

    #[test]
    fn stops_when_user_moves_the_mouse() {
        let mut d = after_first_enter(|w| w.mouse = (10, 10));
        let out = execute(
            &mut d,
            &[set_value(500.0, 500.0, "5"), set_value(300.0, 800.0, "6")],
            &window(),
        );
        assert!(out.error.unwrap().contains("マウス"));
        assert_eq!(out.done.len(), 1);
    }

    #[test]
    fn does_not_type_when_double_click_brought_another_window_to_front() {
        let mut clicks = 0;
        let mut d = FakeDesktop::new().on_event(move |ev, w| {
            if *ev == Ev::Click {
                clicks += 1;
                if clicks == 2 {
                    w.frontmost = Some(42);
                }
            }
        });
        let out = execute(&mut d, &[set_value(500.0, 500.0, "5")], &window());
        assert!(out.error.is_some());
        assert!(
            !d.events
                .iter()
                .any(|e| matches!(e, Ev::Type(_) | Ev::Enter)),
            "別のウィンドウに値を打ち込まない"
        );
        assert!(out.done.is_empty());
    }
}
