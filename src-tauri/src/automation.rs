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

use crate::ai::{ActionKind, AiAction, DoneAction};
use crate::capture::TargetWindow;
use image::RgbaImage;

use crate::geometry::{self, Point, Rect, ScreenWindow};
use crate::screen_check::{self, Refusal};
use crate::value_fields;

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
    fn press_escape(&mut self) -> Result<(), String>;
    /// ウィンドウを撮る (ダブルクリックの前後を比べるため)。
    fn capture_window(&mut self, window: &TargetWindow) -> Result<RgbaImage, String>;
    fn wait(&mut self, d: Duration);
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

        let (x, y) = match a.kind {
            ActionKind::SetValue => {
                // AI の座標そのものではなく、画素で見つけた数値の文字を押す
                let before = desktop.capture_window(window)?;
                let target = locate_field(&before, window, p, a.field)?;
                check_guards(desktop, window, target)?;
                let (x, y) = round_point(target);
                desktop.move_mouse(x, y)?;
                desktop.wait(AFTER_MOVE);
                desktop.click()?;
                desktop.wait(BETWEEN_CLICKS);
                desktop.click()?;
                desktop.wait(AFTER_DOUBLE_CLICK);
                // ダブルクリックで別のウィンドウが開いていないかを確かめてから打ち込む
                ensure_frontmost(desktop, window.pid as i32)?;
                ensure_field_opened(desktop, window, target, &before)?;
                desktop.type_text(a.value.trim())?;
                desktop.wait(AFTER_TYPING);
                desktop.press_enter()?;
                desktop.wait(AFTER_ENTER);
                (x, y)
            }
            ActionKind::SelectBlock => {
                let (x, y) = round_point(p);
                desktop.move_mouse(x, y)?;
                desktop.wait(AFTER_MOVE);
                desktop.click()?;
                desktop.wait(AFTER_SELECT_BLOCK);
                (x, y)
            }
        };
        done.push(to_done(a));
        last_mouse = Some((x, y));
    }
    Ok(())
}

/// 撮った画像から数値を打ち込める行を見つけ、AI が指した高さにいちばん近い行の
/// 数値の文字の真ん中 (画面座標) を返す。その行の番号が AI の答えた `field` と違えば、
/// 読み違いの恐れがあるので操作しない。
pub(crate) fn locate_field(
    img: &RgbaImage,
    window: &TargetWindow,
    p: Point,
    field: u32,
) -> Result<Point, String> {
    let scale_x = img.width() as f64 / window.rect.width;
    let scale_y = img.height() as f64 / window.rect.height;
    let fields = value_fields::find(img, scale_x);
    let (index, f) = value_fields::nearest(&fields, (p.y - window.rect.y) * scale_y)
        .ok_or_else(|| {
            "数値を打ち込める行 (右端に上下の矢印がある行) が画面に見つかりません。ダイアログが開いているか、ブロックが選ばれていない恐れがあるので操作しません".to_string()
        })?;
    if index != field as usize {
        return Err(format!(
            "AI が指した高さにいちばん近い数値の行は上から {index} 番目ですが、AI は {field} 番目と答えました。読み違いの恐れがあるので操作しません"
        ));
    }
    let (vx, vy) = f.value.center();
    Ok(Point {
        x: window.rect.x + vx / scale_x,
        y: window.rect.y + vy / scale_y,
    })
}

/// ダブルクリックの前後を撮り比べ、数値欄の入力が始まったことを確かめる。
/// ダイアログや一覧が開いていたら Escape で閉じ、どちらにしても打ち込まずにエラーにする。
fn ensure_field_opened(
    desktop: &mut dyn Desktop,
    window: &TargetWindow,
    p: Point,
    before: &RgbaImage,
) -> Result<(), String> {
    let after = desktop.capture_window(window)?;
    let scale_x = after.width() as f64 / window.rect.width;
    let scale_y = after.height() as f64 / window.rect.height;
    let center = (
        ((p.x - window.rect.x) * scale_x).max(0.0) as u32,
        ((p.y - window.rect.y) * scale_y).max(0.0) as u32,
    );
    let half = (
        (screen_check::NEAR_HALF_WIDTH * scale_x) as u32,
        (screen_check::NEAR_HALF_HEIGHT * scale_y) as u32,
    );
    let d = screen_check::diff(before, &after, center, half)?;
    match screen_check::check_double_click(&d) {
        Ok(()) => Ok(()),
        Err(r) => {
            if let Refusal::Opened(_) = r {
                desktop.press_escape()?;
            }
            Err(r.message().to_string())
        }
    }
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
        Escape,
    }

    const SCREEN: image::Rgba<u8> = image::Rgba([30, 30, 34, 255]);
    const FIELD: image::Rgba<u8> = image::Rgba([220, 220, 225, 255]);
    const ORANGE: image::Rgba<u8> = image::Rgba([236, 150, 50, 255]);

    /// 数値の行の番号 (1 始まり)。n 番目の行の高さは `field_y(n)` (画像のピクセル)。
    pub const FIELD_ROWS: std::ops::RangeInclusive<u32> = 1..=18;
    /// 数値の文字の真ん中の x (画像のピクセル)。
    pub const VALUE_X: u32 = 904;

    pub fn field_y(n: u32) -> u32 {
        50 + 50 * n
    }

    /// HX Edit のパラメータ欄のように、数値の文字と上下の矢印の行を並べた画面 (1000×1000)。
    pub fn screen_with_fields() -> RgbaImage {
        let mut img = RgbaImage::from_pixel(1000, 1000, SCREEN);
        for n in FIELD_ROWS {
            let y = field_y(n);
            // 数値の文字 (x 890〜918、高さ 10)
            for yy in y - 5..y + 5 {
                for xx in 890..918 {
                    img.put_pixel(xx, yy, ORANGE);
                }
            }
            // 上向きと下向きの三角 (幅 7、高さ 4)
            for (i, width) in [1u32, 3, 5, 7].iter().enumerate() {
                let i = i as u32;
                for dx in 0..*width {
                    let xx = 933 - width / 2 + dx;
                    img.put_pixel(xx, y - 7 + i, ORANGE);
                    img.put_pixel(xx, y + 6 - i, ORANGE);
                }
            }
        }
        img
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
        /// ウィンドウの中身 (ウィンドウが 1000×1000 なら 1 ポイント = 1 ピクセル)。
        pub screen: RgbaImage,
        /// Enter で入力欄が閉じたときに戻る画面。
        pub base: RgbaImage,
        /// クリックすると、その位置に入力欄が開いたように描く。
        pub click_opens_field: bool,
    }

    impl World {
        /// ウィンドウ内の (x, y) ポイントから幅 w・高さ h の矩形を塗る (ダイアログなどの再現に使う)。
        pub fn paint(&mut self, x: u32, y: u32, w: u32, h: u32) {
            for yy in y..(y + h).min(self.screen.height()) {
                for xx in x..(x + w).min(self.screen.width()) {
                    self.screen.put_pixel(xx, yy, FIELD);
                }
            }
        }
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
                    screen: screen_with_fields(),
                    base: screen_with_fields(),
                    click_opens_field: true,
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
            if self.world.click_opens_field {
                let (mx, my) = self.world.mouse;
                let r = self.world.rect.unwrap_or_else(hx_rect);
                let k = self.world.screen.width() as f64 / r.width;
                let (x, y) = (
                    ((mx as f64 - r.x) * k).max(0.0) as u32,
                    ((my as f64 - r.y) * k).max(0.0) as u32,
                );
                self.world
                    .paint(x.saturating_sub(15), y.saturating_sub(6), 30, 12);
            }
            self.record(Ev::Click);
            Ok(())
        }
        fn type_text(&mut self, text: &str) -> Result<(), String> {
            self.record(Ev::Type(text.into()));
            Ok(())
        }
        fn press_enter(&mut self) -> Result<(), String> {
            // 値が確定して入力欄が閉じる
            self.world.screen = self.world.base.clone();
            self.record(Ev::Enter);
            Ok(())
        }
        fn press_escape(&mut self) -> Result<(), String> {
            self.record(Ev::Escape);
            Ok(())
        }
        fn capture_window(&mut self, _window: &TargetWindow) -> Result<RgbaImage, String> {
            Ok(self.world.screen.clone())
        }
        fn wait(&mut self, _d: Duration) {}
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Ev, FakeDesktop, HX_PID, HX_WINDOW, VALUE_X, World, hx_rect};
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

    /// 偽の画面で高さ y (ウィンドウが原点の 1000×1000 なので画面座標と同じ) にある数値の行の番号。
    fn row_at(y: f64) -> u32 {
        (((y - 50.0) / 50.0).round() as u32).clamp(1, 18)
    }

    /// AI が指した点と、その高さにある行の番号を答えた set_value。
    fn set_value(x: f64, y: f64, value: &str) -> AiAction {
        AiAction {
            kind: ActionKind::SetValue,
            block: "Amp".into(),
            parameter: "Bass".into(),
            x,
            y,
            value: value.into(),
            previous_value: "4".into(),
            field: row_at(y),
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
            field: 0,
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

    /// 高さ y の行の数値の文字 (x は VALUE_X) をダブルクリックして打ち込む操作。
    fn set_value_events(y: i32, v: &str) -> Vec<Ev> {
        let x = VALUE_X as i32;
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
        let mut expected = set_value_events(500, "6.0");
        expected.extend(set_value_events(800, "-3"));
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
        assert_eq!(d.input_events(), set_value_events(500, "5"));
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
            set_value_events(500, "5"),
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

    // ---- ダブルクリックの前後の撮り比べ ----

    #[test]
    fn a_dialog_opened_by_the_double_click_is_closed_and_nothing_is_typed() {
        // 実機で起きたこと: 数値欄の上の星マークを押して「Save As Favorite」が開いた
        let mut clicks = 0;
        let mut d = FakeDesktop::new().on_event(move |ev, w| {
            if *ev == Ev::Click {
                clicks += 1;
                if clicks == 2 {
                    w.paint(300, 200, 400, 400);
                }
            }
        });
        let out = execute(
            &mut d,
            &[set_value(500.0, 500.0, "7.0"), set_value(500.0, 600.0, "5")],
            &window(),
        );
        let e = out.error.unwrap();
        assert!(e.contains("周り以外の画面が大きく変わりました"), "{e}");
        assert!(out.done.is_empty(), "変えていないので Undo に積まない");
        let ev = d.input_events();
        assert!(!ev.iter().any(|e| matches!(e, Ev::Type(_))), "打ち込まない");
        assert!(!ev.contains(&Ev::Enter));
        assert_eq!(ev.last(), Some(&Ev::Escape), "開いたものを閉じる");
    }

    #[test]
    fn nothing_is_typed_when_the_double_click_opens_no_field() {
        let mut d = FakeDesktop::new();
        d.world.click_opens_field = false;
        let out = execute(&mut d, &[set_value(500.0, 500.0, "7.0")], &window());
        assert!(out.error.unwrap().contains("入力欄が開きませんでした"));
        let ev = d.input_events();
        assert!(!ev.iter().any(|e| matches!(e, Ev::Type(_))));
        assert!(!ev.contains(&Ev::Escape), "何も開いていないので閉じない");
    }

    #[test]
    fn a_field_opening_in_place_lets_the_value_be_typed() {
        let mut d = FakeDesktop::new();
        let out = execute(&mut d, &[set_value(500.0, 500.0, "7.0")], &window());
        assert_eq!(out.error, None);
        assert!(d.events.contains(&Ev::Type("7.0".into())));
    }

    #[test]
    fn window_position_and_scale_are_used_for_the_field_and_the_check() {
        // ウィンドウを (100, 50) にずらし、撮った画像は 2 倍 (Retina)。
        // AI の点 (600, 500) は画面 (400, 300) = 画像の高さ 500 = 9 番目の行。
        // 押すのは数値の文字の真ん中 (画像 (904, 500)) = 画面 (552, 300)
        let rect = Rect {
            x: 100.0,
            y: 50.0,
            width: 500.0,
            height: 500.0,
        };
        let mut d = FakeDesktop::new();
        d.world.rect = Some(rect);
        let w = TargetWindow { rect, ..window() };
        let mut a = set_value(600.0, 500.0, "7.0");
        a.field = 9;
        let out = execute(&mut d, &[a], &w);
        assert_eq!(
            out.error, None,
            "入力欄の変化はクリックした所の周りとして数える"
        );
        assert_eq!(d.input_events()[0], Ev::Move(552, 300));
    }

    // ---- 数値の行の照合 ----

    #[test]
    fn a_point_between_rows_snaps_to_the_value_of_the_nearest_row() {
        let mut d = FakeDesktop::new();
        let mut a = set_value(300.0, 520.0, "7.0");
        a.field = 9; // 高さ 500 の行
        let out = execute(&mut d, &[a], &window());
        assert_eq!(out.error, None);
        assert_eq!(d.input_events()[0], Ev::Move(VALUE_X as i32, 500));
    }

    #[test]
    fn a_row_number_that_does_not_match_the_point_is_refused() {
        let mut d = FakeDesktop::new();
        let mut a = set_value(500.0, 500.0, "7.0");
        a.field = 3;
        let out = execute(&mut d, &[a], &window());
        let e = out.error.unwrap();
        assert!(e.contains("上から 9 番目") && e.contains("3 番目"), "{e}");
        assert!(d.input_events().is_empty(), "マウスも動かさない");
    }

    #[test]
    fn no_value_rows_on_screen_is_refused() {
        let mut d = FakeDesktop::new();
        d.world.screen = RgbaImage::from_pixel(1000, 1000, image::Rgba([30, 30, 34, 255]));
        let out = execute(&mut d, &[set_value(500.0, 500.0, "7.0")], &window());
        assert!(out.error.unwrap().contains("見つかりません"));
        assert!(d.input_events().is_empty());
    }

    #[test]
    fn on_the_real_screen_a_point_near_the_star_still_clicks_the_gain_value() {
        // 実機で起きたこと: Gain の数値のすぐ上の星マークを押した。
        // 実物の画面 (1440×870 ポイントを 2000 ピクセルに縮めたもの) で、AI が星の高さを
        // 指しても、答えた番号が 1 (Gain) なら Gain の数値 "4.2" の真ん中を押す
        let img = image::open(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/real_hx_dist.png"
        ))
        .unwrap()
        .to_rgba8();
        let rect = Rect {
            x: 0.0,
            y: 30.0,
            width: 1440.0,
            height: 1440.0 * img.height() as f64 / img.width() as f64,
        };
        let w = TargetWindow { rect, ..window() };
        let k = img.width() as f64 / rect.width;
        // 星マークは画像の (1903, 347)
        let star = Point {
            x: rect.x + 1903.0 / k,
            y: rect.y + 347.0 / k,
        };
        let target = locate_field(&img, &w, star, 1).unwrap();
        let (vx, vy) = ((target.x - rect.x) * k, (target.y - rect.y) * k);
        assert!(
            (vx - 1913.0).abs() <= 2.0 && (vy - 389.0).abs() <= 3.0,
            "{vx}, {vy}"
        );
        // 番号を 2 (Tone) と答えていたら、星の高さに近いのは Gain なので食い違いで止める
        assert!(locate_field(&img, &w, star, 2).is_err());
    }
}
