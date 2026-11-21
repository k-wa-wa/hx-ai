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
    let rows = y.saturating_sub(EXTRA_ROWS)..=(y + EXTRA_ROWS).min(img.height() - 1);
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
    let at = px(x);

    // バーの塗られていない部分 (トラック) の色を決める
    let track = if plain(x) {
        *at
    } else {
        let right = (x..img.width()).find(|&i| plain(i));
        let left = (0..x).rev().find(|&i| plain(i));
        let (Some(r), Some(l)) = (right, left) else {
            return Err("スライダーの端が見つかりません".to_string());
        };
        if close(px(r), px(l)) {
            // 塗りの右隣が左隣 (背景) と同じなら、最大まで塗られている
            return checked(img, l + 1, r, r);
        }
        *px(r)
    };

    let on_bar = |i: u32| is_fill(px(i)) || is_text(px(i)) || close(px(i), &track);
    let left = (0..=x).rev().find(|&i| !on_bar(i)).map_or(0, |i| i + 1);
    let right = (x..img.width())
        .find(|&i| !on_bar(i))
        .unwrap_or(img.width());
    if left == 0 || right == img.width() {
        return Err("スライダーの端が見つかりません (バーではない所を指しています)".to_string());
    }
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
}
