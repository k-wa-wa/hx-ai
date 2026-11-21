//! AI が返した操作を編集アプリ (HX Edit / Valeton Suite) に対して実行する。
//!
//! 値の変え方は機種で違う。
//! - HX Edit: 数値欄をダブルクリックして値を打ち込み、Enter で確定する
//! - Valeton Suite (GP-5): スライダーの塗りの端を目標の位置までドラッグし、撮り直して
//!   画素で確かめ、ずれていれば直す
//!
//! 操作の前に毎回ガードレールを確かめ、1 つでも外れたらそこで止める。
//! - 最前面のアプリが操作するアプリである
//! - スクショを撮った時からウィンドウが動いていない
//! - 座標がウィンドウ内 (タイトルバーと縁を除く) にある
//! - その座標で一番手前のウィンドウが HX Edit である
//! - 前の操作からユーザーがマウスを動かしていない
//!
//! OS への操作は `Desktop` トレイトの向こうに置き、テストでは偽物に差し替える。

use std::time::Duration;

use image::RgbaImage;

use crate::ai::{ActionKind, AiAction, DoneAction};
use crate::capture::TargetWindow;
use crate::device::Device;
use crate::geometry::{self, Point, Rect, ScreenWindow};
use crate::slider::{self, Slider};

const AFTER_ACTIVATE: Duration = Duration::from_millis(300);
const AFTER_MOVE: Duration = Duration::from_millis(80);
const BETWEEN_CLICKS: Duration = Duration::from_millis(60);
const AFTER_DOUBLE_CLICK: Duration = Duration::from_millis(200);
const AFTER_TYPING: Duration = Duration::from_millis(80);
const AFTER_ENTER: Duration = Duration::from_millis(250);
const AFTER_SELECT_BLOCK: Duration = Duration::from_millis(500);
const AFTER_PRESS: Duration = Duration::from_millis(50);
const DRAG_STEP: Duration = Duration::from_millis(15);
const AFTER_DRAG: Duration = Duration::from_millis(250);

/// ドラッグを細かく分ける数。一度に飛ぶとドラッグとして扱われないことがあるため。
pub const DRAG_STEPS: i32 = 8;
/// スライダーを合わせ直す回数の上限。
pub const MAX_DRAGS: usize = 3;
/// 合ったとみなす、目標とのずれ (範囲に対する割合)。0〜100 なら 0.6。
pub const SLIDER_TOLERANCE: f64 = 0.006;
/// 変更前の値と、読み取った塗りの位置のずれの許容 (範囲に対する割合)。
/// これより離れていたら、AI が別のスライダーを指したか範囲を読み違えたとみなす。
pub const PREVIOUS_TOLERANCE: f64 = 0.03;
/// 次のドラッグで多めに動かす量の上限 (バーの幅に対する割合)。測り違えて大きく行き過ぎないため。
pub const MAX_LOST: f64 = 0.1;

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
    /// 左ボタンを押す / 離す (ドラッグ用)。
    fn press(&mut self) -> Result<(), String>;
    fn release(&mut self) -> Result<(), String>;
    /// ウィンドウを撮る (スライダーの値を画素で読むため)。
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

fn ensure_frontmost(desktop: &dyn Desktop, window: &TargetWindow) -> Result<(), String> {
    match desktop.frontmost() {
        (Some(p), _) if p == window.pid as i32 => Ok(()),
        (_, name) => Err(format!(
            "最前面のアプリが {} ではありません ({name})。操作を中断しました",
            window.app_name
        )),
    }
}

fn check_guards(desktop: &dyn Desktop, window: &TargetWindow, p: Point) -> Result<(), String> {
    ensure_frontmost(desktop, window)?;
    let current = desktop
        .window_rect(window.window_id)?
        .ok_or_else(|| format!("{} のウィンドウが閉じられました", window.app_name))?;
    geometry::ensure_same_geometry(window.rect, current)?;
    geometry::ensure_inside_window(current, p)?;
    geometry::ensure_topmost_at(&desktop.screen_windows()?, window.window_id, p)?;
    Ok(())
}

/// 検証済みの操作を順に実行する。
pub fn execute(
    desktop: &mut dyn Desktop,
    device: Device,
    actions: &[AiAction],
    window: &TargetWindow,
) -> Outcome {
    let mut done = Vec::new();
    match execute_inner(desktop, device, actions, window, &mut done) {
        Ok(()) => Outcome { done, error: None },
        Err(e) => Outcome {
            done,
            error: Some(e),
        },
    }
}

fn execute_inner(
    desktop: &mut dyn Desktop,
    device: Device,
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

        let mouse = match (a.kind, device) {
            (ActionKind::SetValue, Device::HxStomp) => type_value(desktop, window, a, p)?,
            (ActionKind::SetValue, Device::Gp5) => drag_slider(desktop, window, a, p)?,
            (ActionKind::SelectBlock, _) => {
                let (x, y) = round_point(p);
                desktop.move_mouse(x, y)?;
                desktop.wait(AFTER_MOVE);
                desktop.click()?;
                desktop.wait(AFTER_SELECT_BLOCK);
                (x, y)
            }
        };
        done.push(to_done(a));
        last_mouse = Some(mouse);
    }
    Ok(())
}

/// HX Edit: 数値欄をダブルクリックし、値を打ち込んで Enter で確定する。最後のマウスの位置を返す。
fn type_value(
    desktop: &mut dyn Desktop,
    window: &TargetWindow,
    a: &AiAction,
    p: Point,
) -> Result<(i32, i32), String> {
    let (x, y) = round_point(p);
    desktop.move_mouse(x, y)?;
    desktop.wait(AFTER_MOVE);
    desktop.click()?;
    desktop.wait(BETWEEN_CLICKS);
    desktop.click()?;
    desktop.wait(AFTER_DOUBLE_CLICK);
    // ダブルクリックで別のウィンドウが開いていないかを確かめてから打ち込む
    ensure_frontmost(desktop, window)?;
    desktop.type_text(a.value.trim())?;
    desktop.wait(AFTER_TYPING);
    desktop.press_enter()?;
    desktop.wait(AFTER_ENTER);
    Ok((x, y))
}

/// 画面座標とウィンドウを撮った画像のピクセル座標の変換。
struct ImageMap {
    window: Rect,
    scale_x: f64,
    scale_y: f64,
}

impl ImageMap {
    fn new(window: Rect, img: &RgbaImage) -> Self {
        Self {
            window,
            scale_x: img.width() as f64 / window.width,
            scale_y: img.height() as f64 / window.height,
        }
    }
    fn to_image(&self, p: Point) -> (u32, u32) {
        (
            ((p.x - self.window.x) * self.scale_x).max(0.0) as u32,
            ((p.y - self.window.y) * self.scale_y).max(0.0) as u32,
        )
    }
    fn to_screen_x(&self, ix: f64) -> f64 {
        self.window.x + ix / self.scale_x
    }
}

fn read_slider(
    desktop: &mut dyn Desktop,
    window: &TargetWindow,
    p: Point,
) -> Result<(Slider, ImageMap), String> {
    let img = desktop.capture_window(window)?;
    let map = ImageMap::new(window.rect, &img);
    let (ix, iy) = map.to_image(p);
    Ok((slider::read(&img, ix, iy)?, map))
}

fn number(s: &str, what: &str) -> Result<f64, String> {
    s.trim()
        .parse()
        .map_err(|_| format!("{what}が数値ではありません: {s:?}"))
}

/// Valeton Suite: スライダーの塗りの端を目標の位置までドラッグし、撮り直して確かめる。
/// ずれていれば `MAX_DRAGS` 回まで直す。最後のマウスの位置を返す。
fn drag_slider(
    desktop: &mut dyn Desktop,
    window: &TargetWindow,
    a: &AiAction,
    p: Point,
) -> Result<(i32, i32), String> {
    let name = format!("{} / {}", a.block, a.parameter);
    let (Some(min), Some(max)) = (a.min, a.max) else {
        return Err(format!("{name}: スライダーの範囲 (min / max) がありません"));
    };
    let target = number(&a.value, "値")?;
    let previous = number(&a.previous_value, "変更前の値")?;
    let goal = slider::fraction_of(target, min, max);

    let (mut s, map) = read_slider(desktop, window, p)?;
    let expected = slider::fraction_of(previous, min, max);
    if (s.fraction() - expected).abs() > PREVIOUS_TOLERANCE {
        return Err(format!(
            "{name}: スライダーの塗りが {:.0}% で、変更前の値 {previous} (範囲 {min}〜{max}) の位置 {:.0}% と合いません。別のスライダーを指しているか範囲を読み違えた恐れがあるので、操作しません",
            s.fraction() * 100.0,
            expected * 100.0
        ));
    }

    let y = p.y.round() as i32;
    let mut mouse = round_point(p);
    // ドラッグと認識されるまでの遊びなどで、動かした量より塗りが動かないことがある。
    // 前のドラッグで足りなかった量 (画像のピクセル) を覚えておき、次はその分だけ多めに動かす
    let mut lost = 0.0;
    // 合ったかの確認を 1 か所にするため、最後のドラッグの後の確認の分だけ 1 回多く回す
    for attempt in 0..=MAX_DRAGS {
        if (s.fraction() - goal).abs() <= SLIDER_TOLERANCE {
            return Ok(mouse);
        }
        if attempt == MAX_DRAGS {
            break;
        }
        if attempt > 0 {
            geometry::ensure_mouse_untouched(mouse, desktop.mouse_location()?)?;
        }
        let start = s.fill_end as f64;
        let goal_x = s.x_at(goal);
        let up = goal_x > start;
        let aim = if up { goal_x + lost } else { goal_x - lost };
        let from = map.to_screen_x(start).round() as i32;
        let to = map.to_screen_x(aim).round() as i32;
        check_guards(desktop, window, p)?;
        drag(desktop, y, from, to)?;
        mouse = (to, y);
        s = read_slider(desktop, window, p)?.0;
        // 動かした向きに測った、塗りが実際に動いた量と、動かしたかった量
        let end = s.fill_end as f64;
        let (moved, wanted) = if up {
            (end - start, goal_x - start)
        } else {
            (start - end, start - goal_x)
        };
        lost = (wanted + lost - moved).clamp(0.0, MAX_LOST * (s.right - s.left) as f64);
    }
    Err(format!(
        "{name}: {MAX_DRAGS} 回ドラッグしても {target} に合わせられませんでした (今はおよそ {:.1})",
        slider::value_at(s.fraction(), min, max)
    ))
}

/// 高さ `y` で、左ボタンを押したまま x を `from` から `to` まで細かく分けて動かす。
fn drag(desktop: &mut dyn Desktop, y: i32, from: i32, to: i32) -> Result<(), String> {
    desktop.move_mouse(from, y)?;
    desktop.wait(AFTER_MOVE);
    desktop.press()?;
    desktop.wait(AFTER_PRESS);
    let result = (1..=DRAG_STEPS).try_for_each(|i| {
        desktop.move_mouse(from + (to - from) * i / DRAG_STEPS, y)?;
        desktop.wait(DRAG_STEP);
        Ok::<(), String>(())
    });
    // 途中で失敗してもボタンは離す (押したままにしない)
    desktop.release()?;
    desktop.wait(AFTER_DRAG);
    result
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
        Press,
        Release,
    }

    /// 偽のスライダーがドラッグにどう反応するか。
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum DragModel {
        /// 塗りの端がマウスの位置に合う。
        Absolute,
        /// 動かした量だけ変わるが、最初の `slop` ポイントは動かない (ドラッグと認識されるまでの遊び)。
        Relative { slop: f64 },
        /// ドラッグに反応しない。
        Ignore,
    }

    /// Valeton Suite の偽のスライダー (画面座標)。
    #[derive(Debug, Clone, PartialEq)]
    pub struct FakeSlider {
        pub rect: Rect,
        pub fraction: f64,
    }

    /// ドラッグ中の状態: 押した位置、押した時の割合、押したスライダー。
    #[derive(Debug, Clone)]
    struct Grab {
        x: f64,
        fraction: f64,
        slider: Option<usize>,
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
        pub sliders: Vec<FakeSlider>,
        pub drag_model: DragModel,
        /// 撮った画像の 1 ポイントあたりのピクセル数 (Retina なら 2)。
        pub scale: u32,
        grab: Option<Grab>,
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
                    sliders: vec![],
                    drag_model: DragModel::Absolute,
                    scale: 2,
                    grab: None,
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

        /// 押したままの移動に合わせてスライダーを動かす。
        fn drag_to(&mut self, x: f64) {
            let w = &mut self.world;
            let Some(Grab {
                x: x0,
                fraction,
                slider: Some(i),
            }) = w.grab.clone()
            else {
                return;
            };
            let s = &mut w.sliders[i];
            let f = match w.drag_model {
                DragModel::Absolute => (x - s.rect.x) / s.rect.width,
                DragModel::Relative { slop } => {
                    let dx = x - x0;
                    let moved = (dx.abs() - slop).max(0.0) * dx.signum();
                    fraction + moved / s.rect.width
                }
                DragModel::Ignore => fraction,
            };
            s.fraction = f.clamp(0.0, 1.0);
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
            self.drag_to(x as f64);
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
        fn press(&mut self) -> Result<(), String> {
            let (mx, my) = self.world.mouse;
            let (x, y) = (mx as f64, my as f64);
            let slider = self.world.sliders.iter().position(|s| {
                x >= s.rect.x
                    && x <= s.rect.x + s.rect.width
                    && y >= s.rect.y
                    && y <= s.rect.y + s.rect.height
            });
            let fraction = slider.map_or(0.0, |i| self.world.sliders[i].fraction);
            self.world.grab = Some(Grab {
                x,
                fraction,
                slider,
            });
            self.record(Ev::Press);
            Ok(())
        }
        fn release(&mut self) -> Result<(), String> {
            self.world.grab = None;
            self.record(Ev::Release);
            Ok(())
        }
        fn capture_window(&mut self, window: &TargetWindow) -> Result<RgbaImage, String> {
            use crate::slider::draw;
            let k = self.world.scale as f64;
            let r = window.rect;
            let mut img = draw::canvas((r.width * k) as u32, (r.height * k) as u32);
            for s in &self.world.sliders {
                draw::slider(
                    &mut img,
                    ((s.rect.x - r.x) * k) as u32,
                    ((s.rect.x - r.x + s.rect.width) * k) as u32,
                    ((s.rect.y - r.y) * k) as u32,
                    ((s.rect.y - r.y + s.rect.height) * k) as u32,
                    s.fraction,
                );
            }
            Ok(img)
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
            min: None,
            max: None,
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
            min: None,
            max: None,
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
            Device::HxStomp,
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
        let out = execute(
            &mut d,
            Device::HxStomp,
            &[select_block(580.0, 190.0)],
            &window(),
        );
        assert_eq!(out.error, None);
        assert_eq!(d.input_events(), vec![Ev::Move(580, 190), Ev::Click]);
        assert_eq!(out.done[0].kind, ActionKind::SelectBlock);
    }

    #[test]
    fn empty_actions_do_nothing() {
        let mut d = FakeDesktop::new();
        let out = execute(&mut d, Device::HxStomp, &[], &window());
        assert_eq!(out.error, None);
        assert!(d.events.is_empty());
    }

    #[test]
    fn without_accessibility_nothing_happens() {
        let mut d = FakeDesktop::new();
        d.world.accessibility = false;
        let out = execute(
            &mut d,
            Device::HxStomp,
            &[set_value(500.0, 500.0, "5")],
            &window(),
        );
        assert!(out.error.unwrap().contains("アクセシビリティ"));
        assert!(d.events.is_empty());
    }

    #[test]
    fn invalid_point_anywhere_stops_before_activation() {
        let mut d = FakeDesktop::new();
        let out = execute(
            &mut d,
            Device::HxStomp,
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
        let out = execute(
            &mut d,
            Device::HxStomp,
            &[set_value(500.0, 500.0, "5")],
            &window(),
        );
        assert!(out.error.is_some());
        assert!(d.input_events().is_empty());
    }

    #[test]
    fn stops_when_another_app_comes_to_front() {
        let mut d = after_first_enter(|w| w.frontmost = Some(42));
        let out = execute(
            &mut d,
            Device::HxStomp,
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
            Device::HxStomp,
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
            Device::HxStomp,
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
            Device::HxStomp,
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
            Device::HxStomp,
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
        let out = execute(
            &mut d,
            Device::HxStomp,
            &[set_value(500.0, 500.0, "5")],
            &window(),
        );
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

#[cfg(test)]
mod gp5_tests {
    //! Valeton Suite (GP-5) のスライダーをドラッグで合わせる操作のテスト。

    use super::fake::{DragModel, Ev, FakeDesktop, FakeSlider, HX_PID, HX_WINDOW, hx_rect};
    use super::*;

    fn window() -> TargetWindow {
        TargetWindow {
            window_id: HX_WINDOW,
            pid: HX_PID as u32,
            app_name: "Valeton Suite".into(),
            title: "Valeton Suite".into(),
            rect: hx_rect(),
        }
    }

    /// x 500〜950、y 140〜170 (画面座標) のスライダー 1 本。
    fn desktop(fraction: f64, model: DragModel) -> FakeDesktop {
        let mut d = FakeDesktop::new();
        d.world.sliders = vec![FakeSlider {
            rect: Rect {
                x: 500.0,
                y: 140.0,
                width: 450.0,
                height: 30.0,
            },
            fraction,
        }];
        d.world.drag_model = model;
        d
    }

    fn slide(prev: &str, value: &str) -> AiAction {
        AiAction {
            kind: ActionKind::SetValue,
            block: "AMP".into(),
            parameter: "Gain".into(),
            x: 800.0,
            y: 155.0,
            value: value.into(),
            previous_value: prev.into(),
            min: Some(0.0),
            max: Some(100.0),
        }
    }

    fn run(d: &mut FakeDesktop, a: AiAction) -> Outcome {
        execute(d, Device::Gp5, &[a], &window())
    }

    fn fraction(d: &FakeDesktop) -> f64 {
        d.world.sliders[0].fraction
    }

    fn drags(d: &FakeDesktop) -> usize {
        d.events.iter().filter(|e| **e == Ev::Press).count()
    }

    #[test]
    fn drags_from_the_fill_edge_to_the_target() {
        let mut d = desktop(0.15, DragModel::Absolute);
        let out = run(&mut d, slide("15", "45"));
        assert_eq!(out.error, None);
        assert!(
            (fraction(&d) - 0.45).abs() <= SLIDER_TOLERANCE,
            "{}",
            fraction(&d)
        );
        assert_eq!(out.done.len(), 1);
        assert_eq!(out.done[0].value, "45");
        assert_eq!(drags(&d), 1, "1 回で合う");
        // 塗りの端 (500 + 450 * 0.15 = 567.5) から目標 (500 + 450 * 0.45 = 702.5) へ
        let ev = d.input_events();
        let press = ev.iter().position(|e| *e == Ev::Press).unwrap();
        assert_eq!(ev[press - 1], Ev::Move(568, 155));
        assert_eq!(ev[ev.len() - 2], Ev::Move(703, 155));
        assert_eq!(ev[ev.len() - 1], Ev::Release);
        let moves_while_pressed = ev[press..]
            .iter()
            .filter(|e| matches!(e, Ev::Move(..)))
            .count();
        assert_eq!(
            moves_while_pressed, DRAG_STEPS as usize,
            "細かく分けて動かす"
        );
    }

    #[test]
    fn corrects_when_the_first_drag_falls_short() {
        // ドラッグと認識されるまでの遊びで、最初の数ポイントは動かない
        let mut d = desktop(0.15, DragModel::Relative { slop: 6.0 });
        let out = run(&mut d, slide("15", "45"));
        assert_eq!(out.error, None);
        assert!(
            (fraction(&d) - 0.45).abs() <= SLIDER_TOLERANCE,
            "{}",
            fraction(&d)
        );
        assert!(drags(&d) >= 2, "撮り直して直した");
        assert!(drags(&d) <= MAX_DRAGS);
    }

    #[test]
    fn moves_down_as_well_as_up() {
        let mut d = desktop(0.8, DragModel::Absolute);
        let out = run(&mut d, slide("80", "60"));
        assert_eq!(out.error, None);
        assert!((fraction(&d) - 0.6).abs() <= SLIDER_TOLERANCE);
    }

    #[test]
    fn already_at_the_target_needs_no_drag() {
        let mut d = desktop(0.45, DragModel::Absolute);
        let out = run(&mut d, slide("45", "45"));
        assert_eq!(out.error, None);
        assert_eq!(drags(&d), 0);
    }

    #[test]
    fn gives_up_when_the_slider_does_not_follow() {
        let mut d = desktop(0.15, DragModel::Ignore);
        let out = run(&mut d, slide("15", "45"));
        let e = out.error.unwrap();
        assert!(
            e.contains("合わせられませんでした") && e.contains("およそ 15"),
            "{e}"
        );
        assert!(out.done.is_empty(), "変わっていないので Undo に積まない");
        assert_eq!(drags(&d), MAX_DRAGS);
    }

    #[test]
    fn refuses_when_the_fill_does_not_match_the_previous_value() {
        // AI が「変更前は 60」と読んだが、塗りは 15%
        let mut d = desktop(0.15, DragModel::Absolute);
        let out = run(&mut d, slide("60", "45"));
        let e = out.error.unwrap();
        assert!(
            e.contains("合いません") && e.contains("15%") && e.contains("60%"),
            "{e}"
        );
        assert_eq!(drags(&d), 0, "何も動かさない");
        assert!((fraction(&d) - 0.15).abs() < 1e-9);
    }

    #[test]
    fn previous_value_within_tolerance_is_accepted() {
        let mut d = desktop(0.15, DragModel::Absolute);
        let out = run(&mut d, slide("17", "45"));
        assert_eq!(out.error, None, "2% のずれは読み取りの誤差とみなす");
    }

    #[test]
    fn refuses_without_a_range_or_a_numeric_previous_value() {
        let mut a = slide("15", "45");
        a.max = None;
        let mut d = desktop(0.15, DragModel::Absolute);
        assert!(run(&mut d, a).error.unwrap().contains("min / max"));
        let mut d = desktop(0.15, DragModel::Absolute);
        assert!(
            run(&mut d, slide("?", "45"))
                .error
                .unwrap()
                .contains("変更前の値")
        );
        assert_eq!(drags(&d), 0);
    }

    #[test]
    fn refuses_when_the_point_is_not_on_a_slider() {
        let mut d = desktop(0.15, DragModel::Absolute);
        let mut a = slide("15", "45");
        a.y = 500.0;
        assert!(run(&mut d, a).error.unwrap().contains("スライダー"));
        assert_eq!(drags(&d), 0);
    }

    #[test]
    fn stops_when_the_user_moves_the_mouse_between_drags() {
        let mut d = desktop(0.15, DragModel::Relative { slop: 6.0 }).on_event(|ev, w| {
            if *ev == Ev::Release {
                w.mouse = (10, 10);
            }
        });
        let out = run(&mut d, slide("15", "45"));
        assert!(out.error.unwrap().contains("マウス"));
        assert_eq!(drags(&d), 1);
    }

    #[test]
    fn checks_the_guards_again_before_a_correcting_drag() {
        let mut d = desktop(0.15, DragModel::Relative { slop: 6.0 }).on_event(|ev, w| {
            if *ev == Ev::Release {
                w.frontmost = Some(42);
            }
        });
        let out = run(&mut d, slide("15", "45"));
        assert!(out.error.unwrap().contains("Valeton Suite ではありません"));
        assert_eq!(drags(&d), 1, "直しのドラッグはしない");
    }

    #[test]
    fn does_not_overshoot_when_the_slider_follows_exactly() {
        // 遊びの無いスライダーでは、多めに動かす量は 0 のまま
        let mut d = desktop(0.15, DragModel::Absolute);
        run(&mut d, slide("15", "45"));
        assert!((fraction(&d) - 0.45).abs() <= SLIDER_TOLERANCE);
        assert_eq!(drags(&d), 1);
    }

    #[test]
    fn releases_the_button_even_if_moving_fails_midway() {
        struct Broken(FakeDesktop, usize);
        impl Desktop for Broken {
            fn accessibility_granted(&self) -> bool {
                true
            }
            fn activate(&mut self, pid: i32) -> Result<(), String> {
                self.0.activate(pid)
            }
            fn frontmost(&self) -> (Option<i32>, String) {
                self.0.frontmost()
            }
            fn window_rect(&self, id: u32) -> Result<Option<Rect>, String> {
                self.0.window_rect(id)
            }
            fn screen_windows(&self) -> Result<Vec<ScreenWindow>, String> {
                self.0.screen_windows()
            }
            fn mouse_location(&self) -> Result<(i32, i32), String> {
                self.0.mouse_location()
            }
            fn move_mouse(&mut self, x: i32, y: i32) -> Result<(), String> {
                self.1 += 1;
                if self.1 > 3 {
                    return Err("マウスを動かせません".into());
                }
                self.0.move_mouse(x, y)
            }
            fn click(&mut self) -> Result<(), String> {
                self.0.click()
            }
            fn type_text(&mut self, t: &str) -> Result<(), String> {
                self.0.type_text(t)
            }
            fn press_enter(&mut self) -> Result<(), String> {
                self.0.press_enter()
            }
            fn press(&mut self) -> Result<(), String> {
                self.0.press()
            }
            fn release(&mut self) -> Result<(), String> {
                self.0.release()
            }
            fn capture_window(&mut self, w: &TargetWindow) -> Result<RgbaImage, String> {
                self.0.capture_window(w)
            }
            fn wait(&mut self, _d: Duration) {}
        }
        let mut b = Broken(desktop(0.15, DragModel::Absolute), 0);
        let out = execute(&mut b, Device::Gp5, &[slide("15", "45")], &window());
        assert!(out.error.unwrap().contains("動かせません"));
        assert_eq!(b.0.events.last(), Some(&Ev::Release), "押したままにしない");
    }

    /// ウィンドウを (100, 50) にずらし、スライダーを x 600〜1050、y 190〜220 に置く。
    fn offset_desktop(fraction: f64, model: DragModel) -> (FakeDesktop, TargetWindow) {
        let rect = Rect {
            x: 100.0,
            y: 50.0,
            width: 1000.0,
            height: 1000.0,
        };
        let mut d = FakeDesktop::new();
        d.world.rect = Some(rect);
        d.world.sliders = vec![FakeSlider {
            rect: Rect {
                x: 600.0,
                y: 190.0,
                width: 450.0,
                height: 30.0,
            },
            fraction,
        }];
        d.world.drag_model = model;
        (d, TargetWindow { rect, ..window() })
    }

    /// n 回目 (0 始まり) のドラッグの始点と終点。
    fn drag_ends(d: &FakeDesktop, n: usize) -> (Ev, Ev) {
        let ev = d.input_events();
        let press = ev
            .iter()
            .enumerate()
            .filter(|(_, e)| **e == Ev::Press)
            .nth(n)
            .unwrap()
            .0;
        let release = press + ev[press..].iter().position(|e| *e == Ev::Release).unwrap();
        (ev[press - 1].clone(), ev[release - 1].clone())
    }

    #[test]
    fn the_shortfall_of_a_drag_is_added_to_the_next_one() {
        // 画像は 2 倍。塗りの端 1135 → 目標 1405 (ピクセル)。遊び 6 ポイントで 258 しか動かず、12 足りない
        let (mut d, w) = offset_desktop(0.15, DragModel::Relative { slop: 6.0 });
        let out = execute(&mut d, Device::Gp5, &[slide("15", "45")], &w);
        assert_eq!(out.error, None);
        assert_eq!(drag_ends(&d, 0), (Ev::Move(668, 205), Ev::Move(803, 205)));
        // 2 回目は残り 12 に足りなかった 12 を足して 24 ピクセル (12 ポイント) 動かす
        assert_eq!(drag_ends(&d, 1), (Ev::Move(797, 205), Ev::Move(809, 205)));
        assert_eq!(drags(&d), 2);
    }

    #[test]
    fn the_shortfall_is_added_in_the_direction_of_the_drag() {
        let (mut d, w) = offset_desktop(0.8, DragModel::Relative { slop: 6.0 });
        let out = execute(&mut d, Device::Gp5, &[slide("80", "50")], &w);
        assert_eq!(out.error, None);
        assert_eq!(drag_ends(&d, 0), (Ev::Move(960, 205), Ev::Move(825, 205)));
        assert_eq!(drag_ends(&d, 1), (Ev::Move(831, 205), Ev::Move(819, 205)));
    }

    #[test]
    fn the_shortfall_added_is_capped() {
        // 遊びが大きい (60 ポイント)。足りなかった 120 ピクセルのうち、バーの幅の 1 割 (90) だけを足す
        let (mut d, w) = offset_desktop(0.15, DragModel::Relative { slop: 60.0 });
        execute(&mut d, Device::Gp5, &[slide("15", "45")], &w);
        assert_eq!(drag_ends(&d, 1), (Ev::Move(743, 205), Ev::Move(848, 205)));
        // 2 回目も 90 しか動かず、足りなかった分は 120 → 上限の 90 のまま 3 回目に足す
        assert_eq!(drag_ends(&d, 2), (Ev::Move(788, 205), Ev::Move(848, 205)));
    }

    #[test]
    fn hx_stomp_still_types_values() {
        let mut d = desktop(0.15, DragModel::Absolute);
        let mut a = slide("15", "45");
        a.y = 500.0;
        let out = execute(&mut d, Device::HxStomp, &[a], &window());
        assert_eq!(out.error, None);
        assert!(d.events.contains(&Ev::Type("45".into())));
        assert_eq!(drags(&d), 0);
    }
}
