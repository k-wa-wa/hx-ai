//! Valeton Suite (GP-5) の横長のスライダーを画素から読む。OS には触らない。
//!
//! スライダーは横長のバーで、左端が最小、右端が最大。今の値までが青く塗られ、
//! バーの上に白い文字 (パラメータ名と値) が乗っている。

use image::{Rgba, RgbaImage};

/// 読み取ったスライダー (画像のピクセル座標)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slider {
    /// バーの左端 (この x からバー)。
    pub left: u32,
    /// バーの右端 (この x の手前までがバー)。
    pub right: u32,
    /// 塗りの右端 (この x の手前までが塗り)。値が最小なら `left`。
    pub fill_end: u32,
}

impl Slider {
    /// 塗られている割合 (0〜1)。
    pub fn fraction(&self) -> f64 {
        (self.fill_end - self.left) as f64 / (self.right - self.left) as f64
    }

    /// 割合 `f` に当たる x (画像のピクセル座標)。
    pub fn x_at(&self, f: f64) -> f64 {
        self.left as f64 + f.clamp(0.0, 1.0) * (self.right - self.left) as f64
    }
}

/// これより短いものはスライダーとみなさない (ピクセル)。
pub const MIN_WIDTH: u32 = 40;

/// 同じ色とみなす各チャンネルの差。
const TOLERANCE: i32 = 14;

/// 色を取るときに、同じ色が続いていることを確かめる画素の数 (この数 + 1 個続く)。
const STEADY: u32 = 2;

/// 指した点から塗りかトラックを探す範囲 (ピクセル)。値の文字の幅より広くする。
const SEARCH: u32 = 80;

/// 塗りの青。青が赤・緑よりはっきり強い。
fn is_fill(p: &Rgba<u8>) -> bool {
    let [r, g, b, _] = p.0;
    b >= 120 && b as i32 > r as i32 + 50 && b as i32 > g as i32 + 25
}

/// バーの上の白い文字。
fn is_text(p: &Rgba<u8>) -> bool {
    let [r, g, b, _] = p.0;
    r.min(g).min(b) >= 150 && !is_fill(p)
}

fn close(a: &Rgba<u8>, b: &Rgba<u8>) -> bool {
    a.0.iter()
        .zip(b.0.iter())
        .take(3)
        .all(|(x, y)| (*x as i32 - *y as i32).abs() <= TOLERANCE)
}

/// 上下に読み足す行の数。白い文字が塗りの端に重なると、その行では端が隠れるため。
const EXTRA_ROWS: u32 = 8;

/// (x, y) にあるスライダーを読む。(x, y) はバーの上ならどこでもよい。
/// 指した行と、その上下の行のうち両端が同じものを読み、塗りの端はいちばん右を採る。
pub fn read(img: &RgbaImage, x: u32, y: u32) -> Result<Slider, String> {
    let base = read_row(img, x, y)?;
    // 画像の外の行は read_row がエラーにするので、範囲はそのままでよい
    let rows = y.saturating_sub(EXTRA_ROWS)..=y + EXTRA_ROWS;
    let fill_end = rows
        .filter_map(|r| read_row(img, x, r).ok())
        .filter(|s| s.left == base.left && s.right == base.right)
        .map(|s| s.fill_end)
        .max()
        .unwrap_or(base.fill_end);
    Ok(Slider { fill_end, ..base })
}

/// 1 行だけを読む。
fn read_row(img: &RgbaImage, x: u32, y: u32) -> Result<Slider, String> {
    if x >= img.width() || y >= img.height() {
        return Err(format!("指した位置 ({x}, {y}) が画像の外です"));
    }
    let px = |x: u32| img.get_pixel(x, y);
    let plain = |x: u32| !is_fill(px(x)) && !is_text(px(x));
    // 縁はなめらかに描かれていて中間色の画素があるので、同じ色が続く所から色を取る
    let steady = |i: u32| {
        (i + STEADY < img.width()) && (0..=STEADY).all(|k| plain(i + k) && close(px(i + k), px(i)))
    };

    // 指した点の周りが塗りかトラックかを決める (文字や縁の中間色は飛ばし、近い方から見る)
    let near = (0..SEARCH).find_map(|d| {
        [x.checked_add(d), x.checked_sub(d)]
            .into_iter()
            .flatten()
            .filter(|&i| i < img.width())
            .find_map(|i| {
                if is_fill(px(i)) {
                    Some(None)
                } else if steady(i) {
                    Some(Some(i))
                } else {
                    None
                }
            })
    });
    let track = match near {
        None => return Err("指した位置の周りにスライダーが見つかりません".to_string()),
        // トラックの上なら、その色がトラックの色
        Some(Some(i)) => *px(i),
        // 塗りの上なら、塗りの右隣がトラック。ただし左隣 (背景) と同じなら最大まで塗られている
        Some(None) => {
            let right = (x..img.width()).find(|&i| steady(i));
            let left = (0..x).rev().find(|&i| i >= STEADY && steady(i - STEADY));
            let (Some(r), Some(l)) = (right, left) else {
                return Err("スライダーの端が見つかりません".to_string());
            };
            if close(px(r), px(l)) {
                return checked(img, l + 1, r, r);
            }
            *px(r)
        }
    };

    let on_bar = |i: u32| is_fill(px(i)) || is_text(px(i)) || close(px(i), &track);
    // 塗りの端などの中間色で途切れないよう、合わない画素が STEADY 個までなら先を見る
    let extent = |xs: &mut dyn Iterator<Item = u32>| {
        let mut last = None;
        let mut misses = 0;
        for i in xs {
            if on_bar(i) {
                last = Some(i);
                misses = 0;
            } else {
                misses += 1;
                if misses > STEADY {
                    return (last, true);
                }
            }
        }
        (last, false)
    };
    let (Some(first), true) = extent(&mut (0..=x).rev()) else {
        return Err("スライダーの端が見つかりません (バーではない所を指しています)".to_string());
    };
    let (Some(last), true) = extent(&mut (x..img.width())) else {
        return Err("スライダーの端が見つかりません (バーではない所を指しています)".to_string());
    };
    let (left, right) = (first, last + 1);
    let fill_end = (left..right)
        .rev()
        .find(|&i| is_fill(px(i)))
        .map_or(left, |i| i + 1);
    checked(img, left, right, fill_end)
}

fn checked(img: &RgbaImage, left: u32, right: u32, fill_end: u32) -> Result<Slider, String> {
    let width = right.saturating_sub(left);
    if width < MIN_WIDTH || width * 10 > img.width() * 9 {
        return Err(format!(
            "スライダーに見えません (幅 {width} ピクセル)。バーの上を指していないかもしれません"
        ));
    }
    Ok(Slider {
        left,
        right,
        fill_end,
    })
}

/// 値の範囲の中での割合 (0〜1)。
pub fn fraction_of(value: f64, min: f64, max: f64) -> f64 {
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// 割合から値に戻す。
pub fn value_at(fraction: f64, min: f64, max: f64) -> f64 {
    min + fraction * (max - min)
}

#[cfg(test)]
pub mod draw {
    //! テスト用に、Valeton Suite 風のスライダーを描く。

    use super::*;

    pub const BG: Rgba<u8> = Rgba([22, 26, 34, 255]);
    pub const TRACK: Rgba<u8> = Rgba([38, 43, 54, 255]);
    pub const FILL: Rgba<u8> = Rgba([59, 116, 230, 255]);
    pub const TEXT: Rgba<u8> = Rgba([236, 238, 242, 255]);

    /// 背景だけの画像。
    pub fn canvas(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, BG)
    }

    /// (left..right, top..bottom) にスライダーを描く。左端に名前、右端に値の文字を模した白い塊を置く。
    pub fn slider(img: &mut RgbaImage, left: u32, right: u32, top: u32, bottom: u32, f: f64) {
        let fill_end = left + ((right - left) as f64 * f).round() as u32;
        for y in top..bottom {
            for x in left..right {
                img.put_pixel(x, y, if x < fill_end { FILL } else { TRACK });
            }
        }
        let mid = (top + bottom) / 2;
        for y in mid - 3..mid + 3 {
            // 名前 (左) と値 (右) の文字
            for x in left + 12..left + 60 {
                if x % 4 != 0 {
                    img.put_pixel(x, y, TEXT);
                }
            }
            for x in right - 40..right - 12 {
                if x % 3 != 0 {
                    img.put_pixel(x, y, TEXT);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::draw::*;
    use super::*;

    fn image_with(f: f64) -> RgbaImage {
        let mut img = canvas(1000, 200);
        slider(&mut img, 100, 900, 80, 120, f);
        img
    }

    #[test]
    fn reads_bar_ends_and_fill_from_the_track() {
        let s = read(&image_with(0.15), 700, 100).unwrap();
        assert_eq!((s.left, s.right, s.fill_end), (100, 900, 220));
        assert!((s.fraction() - 0.15).abs() < 1e-9);
    }

    #[test]
    fn reads_from_a_point_on_the_fill_or_on_the_text() {
        let img = image_with(0.6);
        for (x, y) in [(150, 85), (30 + 100, 100), (870, 100)] {
            let s = read(&img, x, y).unwrap();
            assert_eq!((s.left, s.right, s.fill_end), (100, 900, 580), "({x}, {y})");
        }
    }

    #[test]
    fn text_on_the_track_does_not_count_as_fill() {
        // 値が小さいと、名前の文字が塗りからはみ出してトラックに乗る
        let s = read(&image_with(0.03), 500, 100).unwrap();
        assert_eq!(s.fill_end, 124);
    }

    #[test]
    fn empty_and_full_sliders() {
        let empty = read(&image_with(0.0), 500, 85).unwrap();
        assert_eq!((empty.left, empty.fill_end), (100, 100));
        assert_eq!(empty.fraction(), 0.0);
        let full = read(&image_with(1.0), 500, 85).unwrap();
        assert_eq!((full.left, full.right, full.fill_end), (100, 900, 900));
        assert_eq!(full.fraction(), 1.0);
    }

    #[test]
    fn pointing_outside_a_bar_is_an_error() {
        let img = image_with(0.5);
        assert!(read(&img, 500, 30).is_err(), "背景");
        assert!(read(&img, 2000, 100).is_err(), "画像の外");
        let mut short = canvas(1000, 200);
        slider(&mut short, 100, 130, 80, 120, 0.5);
        assert!(read(&short, 125, 85).is_err(), "短すぎる");
    }

    #[test]
    fn x_at_and_value_conversions() {
        let s = Slider {
            left: 100,
            right: 900,
            fill_end: 100,
        };
        assert_eq!(s.x_at(0.5), 500.0);
        assert_eq!(s.x_at(1.5), 900.0, "範囲外は端に寄せる");
        assert_eq!(fraction_of(45.0, 0.0, 100.0), 0.45);
        assert_eq!(fraction_of(0.0, -12.0, 12.0), 0.5);
        assert_eq!(value_at(0.25, -12.0, 12.0), -6.0);
        assert_eq!(fraction_of(200.0, 0.0, 100.0), 1.0);
    }

    /// scripts/make_mock_valeton.swift で描いた Valeton Suite 風の画面 (CoreGraphics の
    /// なめらかな縁と、実物の大きさの文字が乗る)。
    #[test]
    fn reads_every_slider_on_the_mock_valeton_screen() {
        let img = image::load_from_memory(include_bytes!("../tests/fixtures/mock_valeton.png"))
            .unwrap()
            .to_rgba8();
        let values = [60.0, 50.0, 70.0, 50.0, 45.0, 65.0];
        for (i, v) in values.iter().enumerate() {
            let y = ((165.0 + 42.0 * i as f64 + 15.0) * 2.0) as u32;
            // 塗りの上・トラックの上・名前の文字の上・値の文字の上
            for x in [1100, 1700, 1000, 1850] {
                let s = read(&img, x, y).unwrap_or_else(|e| panic!("{i} ({x}): {e}"));
                assert!(
                    (s.fraction() - v / 100.0).abs() < 0.005,
                    "{i} ({x}): {} != {v}",
                    s.fraction()
                );
                assert!((973..=976).contains(&s.left), "{i}: left {}", s.left);
                assert!((1874..=1877).contains(&s.right), "{i}: right {}", s.right);
            }
        }
    }

    #[test]
    fn fill_color_boundaries() {
        let c = |r, g, b| is_fill(&Rgba([r, g, b, 255]));
        assert!(c(0, 0, 120) && !c(0, 0, 119), "青の強さ");
        assert!(c(69, 0, 120) && !c(70, 0, 120), "赤より 50 強い");
        assert!(c(0, 94, 120) && !c(0, 95, 120), "緑より 25 強い");
        assert!(c(59, 116, 230), "実物の塗り");
        assert!(!c(38, 43, 54) && !c(236, 238, 242), "トラックと文字");
        let t = |r, g, b| is_text(&Rgba([r, g, b, 255]));
        assert!(t(150, 150, 150) && !t(149, 200, 200));
        assert!(!t(150, 150, 255), "青い色は文字ではない");
    }

    /// 塗りの端 (x 260) に文字が重なり、`hidden` の行では端が見えない画像。
    fn hidden_edge(hidden: std::ops::Range<u32>) -> RgbaImage {
        let mut img = image_with(0.2);
        for y in hidden {
            for x in 240..300 {
                img.put_pixel(x, y, TEXT);
            }
        }
        img
    }

    #[test]
    fn edge_hidden_on_the_row_and_above_is_read_from_rows_below() {
        let s = read(&hidden_edge(80..104), 500, 100).unwrap();
        assert_eq!(s.fill_end, 260);
    }

    #[test]
    fn edge_hidden_on_the_row_and_below_is_read_from_rows_above() {
        let s = read(&hidden_edge(96..120), 500, 100).unwrap();
        assert_eq!(s.fill_end, 260);
    }

    #[test]
    fn rows_of_a_neighbouring_bar_are_not_mixed_in() {
        // すぐ上に、左端は同じで長さの違う別のバーがある (塗りは 90%)
        let mut img = canvas(1000, 200);
        slider(&mut img, 100, 700, 60, 80, 0.9);
        slider(&mut img, 100, 900, 80, 120, 0.2);
        let s = read(&img, 500, 84).unwrap();
        assert_eq!((s.right, s.fill_end), (900, 260));
    }

    #[test]
    fn rows_below_the_image_are_ignored() {
        let mut img = canvas(1000, 120);
        slider(&mut img, 100, 900, 100, 120, 0.3);
        assert_eq!(read(&img, 500, 119).unwrap().fill_end, 340);
    }

    #[test]
    fn point_below_the_image_is_an_error() {
        assert!(read(&image_with(0.5), 500, 200).is_err());
    }

    #[test]
    fn fill_reaching_the_image_edge_is_an_error_not_a_crash() {
        let mut img = canvas(1000, 200);
        slider(&mut img, 100, 1000, 80, 120, 1.0);
        assert!(read(&img, 500, 85).is_err());
        let mut track = canvas(1000, 200);
        slider(&mut track, 100, 1000, 80, 120, 0.1);
        assert!(read(&track, 999, 85).is_err());
    }

    #[test]
    fn a_two_pixel_blend_at_the_fill_edge_does_not_cut_the_bar() {
        let mut img = image_with(0.5);
        // 塗りでもトラックでもない中間色が 2 画素続く
        for y in 80..120 {
            img.put_pixel(500, y, Rgba([45, 62, 100, 255]));
            img.put_pixel(501, y, Rgba([42, 55, 80, 255]));
        }
        let s = read(&img, 300, 85).unwrap();
        assert_eq!((s.left, s.right), (100, 900));
        assert_eq!(s.fill_end, 500);
    }

    #[test]
    fn width_limits() {
        let mut img = canvas(1000, 200);
        slider(&mut img, 100, 100 + MIN_WIDTH, 80, 120, 0.5);
        assert!(read(&img, 110, 85).is_ok(), "幅 40 は読む");
        let mut img = canvas(1000, 200);
        slider(&mut img, 100, 99 + MIN_WIDTH, 80, 120, 0.5);
        assert!(read(&img, 110, 85).is_err(), "幅 39 は読まない");
        let mut img = canvas(1000, 200);
        slider(&mut img, 50, 950, 80, 120, 0.5);
        assert!(read(&img, 500, 85).is_ok(), "画像の 9 割ちょうどは読む");
        let mut img = canvas(1000, 200);
        slider(&mut img, 49, 950, 80, 120, 0.5);
        assert!(read(&img, 500, 85).is_err(), "9 割を超えるのはバーではない");
    }
}
