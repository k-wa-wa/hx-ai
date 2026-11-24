//! HX Edit の画面から、数値を打ち込めるパラメータの行を画素で見つける。OS には触らない。
//!
//! 数値を打ち込める行には、右端に上下の矢印 (上向きと下向きの小さな三角が縦に並んだもの) があり、
//! その左に数値の文字 (例: "4.2", "600 ms") がある。どちらもブロックの色で描かれる。
//! 段階のボタンや Off/On の切り替えの行には矢印が無い。

use image::RgbaImage;

/// 画像のピクセル座標の矩形 (右端・下端は含まない)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Bounds {
    pub fn width(&self) -> u32 {
        self.right - self.left
    }
    pub fn height(&self) -> u32 {
        self.bottom - self.top
    }
    pub fn center(&self) -> (f64, f64) {
        (
            (self.left + self.right) as f64 / 2.0,
            (self.top + self.bottom) as f64 / 2.0,
        )
    }
    fn union(&self, o: &Bounds) -> Bounds {
        Bounds {
            left: self.left.min(o.left),
            top: self.top.min(o.top),
            right: self.right.max(o.right),
            bottom: self.bottom.max(o.bottom),
        }
    }
}

/// 色の付いた画素の塊。
#[derive(Debug, Clone, PartialEq)]
pub struct Blob {
    pub bounds: Bounds,
    /// 行ごとの画素の数 (上から)。三角の向きを見るのに使う。
    pub rows: Vec<u32>,
    pub pixels: u32,
}

/// ブロックの色 (オレンジ・緑など) で描かれた画素か。白・灰色・黒は含めない。
fn is_colored(p: &image::Rgba<u8>) -> bool {
    let [r, g, b, _] = p.0;
    let max = r.max(g).max(b) as i32;
    let min = r.min(g).min(b) as i32;
    max >= 110 && max - min >= 70
}

/// `region` の中の、色の付いた画素の塊を見つける (8 近傍)。
pub fn blobs(img: &RgbaImage, region: Bounds) -> Vec<Blob> {
    let w = region.width() as usize;
    let h = region.height() as usize;
    let mut seen = vec![false; w * h];
    let colored = |x: usize, y: usize| {
        is_colored(img.get_pixel(region.left + x as u32, region.top + y as u32))
    };
    let mut out = Vec::new();
    for sy in 0..h {
        for sx in 0..w {
            if seen[sy * w + sx] || !colored(sx, sy) {
                continue;
            }
            seen[sy * w + sx] = true;
            let mut stack = vec![(sx, sy)];
            let mut pts = Vec::new();
            while let Some((x, y)) = stack.pop() {
                pts.push((x, y));
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                            continue;
                        }
                        let (nx, ny) = (nx as usize, ny as usize);
                        if !seen[ny * w + nx] && colored(nx, ny) {
                            seen[ny * w + nx] = true;
                            stack.push((nx, ny));
                        }
                    }
                }
            }
            let left = pts.iter().map(|p| p.0).min().unwrap() as u32;
            let right = pts.iter().map(|p| p.0).max().unwrap() as u32 + 1;
            let top = pts.iter().map(|p| p.1).min().unwrap() as u32;
            let bottom = pts.iter().map(|p| p.1).max().unwrap() as u32 + 1;
            let mut rows = vec![0u32; (bottom - top) as usize];
            for p in &pts {
                rows[p.1 - top as usize] += 1;
            }
            out.push(Blob {
                bounds: Bounds {
                    left: region.left + left,
                    top: region.top + top,
                    right: region.left + right,
                    bottom: region.top + bottom,
                },
                rows,
                pixels: pts.len() as u32,
            });
        }
    }
    out
}

/// 数値を打ち込める 1 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// 上下の矢印 (2 つの三角を合わせた範囲)。
    pub arrows: Bounds,
    /// 数値の文字 (単位や「▾」を含む)。ここをダブルクリックする。
    pub value: Bounds,
}

/// 検出のしきい値 (ポイント)。実物の HX Edit 3.82 の画面で測った大きさに余裕を持たせてある。
const TRIANGLE_MAX_HEIGHT: f64 = 6.0;
const TRIANGLE_MIN_WIDTH: f64 = 3.0;
const TRIANGLE_MAX_WIDTH: f64 = 9.0;
/// 上下の三角の横位置のずれの許容。
const PAIR_MAX_DX: f64 = 2.0;
/// 上の三角の下端から下の三角の上端までの距離の上限。
const PAIR_MAX_GAP: f64 = 11.0;
/// 矢印と数値の文字の間、文字どうしの間の距離の上限。
const TEXT_TO_ARROWS_MAX_GAP: f64 = 16.0;
const TEXT_MAX_GAP: f64 = 8.0;
/// 探す範囲: ウィンドウの右端からこの幅 (数値の文字は長くても 100 ポイントほど)。
const SEARCH_WIDTH: f64 = 220.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pointing {
    Up,
    Down,
}

/// 三角の向き。いちばん幅の広い行が下半分にあれば上向き、上半分にあれば下向き。
fn triangle(b: &Blob, px_per_pt: f64) -> Option<Pointing> {
    let w = b.bounds.width() as f64 / px_per_pt;
    let h = b.bounds.height() as f64 / px_per_pt;
    if !(TRIANGLE_MIN_WIDTH..=TRIANGLE_MAX_WIDTH).contains(&w) || h > TRIANGLE_MAX_HEIGHT || h >= w
    {
        return None;
    }
    // 縁の中間色で端の行に 1 画素だけ付くことがあるので、単調かではなく、
    // いちばん幅の広い行が下半分にあるか上半分にあるかで決める
    let max = *b.rows.iter().max()?;
    let first_max = b.rows.iter().position(|r| *r == max)? as f64;
    let last_max = b.rows.iter().rposition(|r| *r == max)? as f64;
    let middle = (b.rows.len() - 1) as f64 / 2.0;
    if last_max > middle && first_max >= middle {
        Some(Pointing::Up)
    } else if first_max < middle && last_max <= middle {
        Some(Pointing::Down)
    } else {
        None
    }
}

/// 画像 (ウィンドウ全体) から、数値を打ち込める行を上から順に見つける。
/// `px_per_pt` は 1 ポイントあたりのピクセル数 (Retina なら 2)。
pub fn find(img: &RgbaImage, px_per_pt: f64) -> Vec<Field> {
    let (w, h) = img.dimensions();
    let strip = ((SEARCH_WIDTH * px_per_pt) as u32).min(w);
    let all = blobs(
        img,
        Bounds {
            left: w - strip,
            top: 0,
            right: w,
            bottom: h,
        },
    );
    let pt = |v: f64| v * px_per_pt;
    let ups: Vec<&Blob> = all
        .iter()
        .filter(|b| triangle(b, px_per_pt) == Some(Pointing::Up))
        .collect();
    let downs: Vec<&Blob> = all
        .iter()
        .filter(|b| triangle(b, px_per_pt) == Some(Pointing::Down))
        .collect();
    let mut fields = Vec::new();
    for u in &ups {
        let (ux, _) = u.bounds.center();
        let Some(d) = downs.iter().find(|d| {
            (d.bounds.center().0 - ux).abs() <= pt(PAIR_MAX_DX)
                && d.bounds.top >= u.bounds.bottom
                && ((d.bounds.top - u.bounds.bottom) as f64) <= pt(PAIR_MAX_GAP)
        }) else {
            continue;
        };
        let arrows = u.bounds.union(&d.bounds);
        if let Some(value) = value_text(&all, &arrows, px_per_pt) {
            fields.push(Field { arrows, value });
        }
    }
    fields.sort_by_key(|f| f.arrows.top);
    fields
}

/// 矢印の左にある数値の文字。矢印の高さに重なる塊を、右から順に間が空きすぎるまでつなぐ。
fn value_text(all: &[Blob], arrows: &Bounds, px_per_pt: f64) -> Option<Bounds> {
    let mut beside: Vec<&Blob> = all
        .iter()
        .filter(|b| {
            b.bounds.right <= arrows.left
                && b.bounds.bottom > arrows.top
                && b.bounds.top < arrows.bottom
        })
        .collect();
    beside.sort_by_key(|b| std::cmp::Reverse(b.bounds.right));
    let mut text: Option<Bounds> = None;
    let mut edge = arrows.left;
    for b in beside {
        let limit = if text.is_none() {
            TEXT_TO_ARROWS_MAX_GAP
        } else {
            TEXT_MAX_GAP
        };
        if (edge.saturating_sub(b.bounds.right)) as f64 > limit * px_per_pt {
            break;
        }
        text = Some(text.map_or(b.bounds, |t| t.union(&b.bounds)));
        edge = edge.min(b.bounds.left);
    }
    text
}

/// AI が指した点 (画像のピクセル) にいちばん近い行 (縦の位置で比べる) と、その番号 (1 始まり)。
pub fn nearest(fields: &[Field], y: f64) -> Option<(usize, &Field)> {
    fields
        .iter()
        .enumerate()
        .min_by(|a, b| {
            let da = (a.1.arrows.center().1 - y).abs();
            let db = (b.1.arrows.center().1 - y).abs();
            da.total_cmp(&db)
        })
        .map(|(i, f)| (i + 1, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の画像は、1440 ポイントのウィンドウを横 2000 ピクセルに縮めたもの。
    const PX_PER_PT: f64 = 2000.0 / 1440.0;

    fn real(name: &str) -> Vec<Field> {
        let path = format!("{}/tests/fixtures/{name}.png", env!("CARGO_MANIFEST_DIR"));
        find(&image::open(&path).unwrap().to_rgba8(), PX_PER_PT)
    }

    /// 各行の矢印の高さの中心。期待値と 1 ピクセルまでのずれは許す。
    fn assert_rows(fields: &[Field], expected: &[f64]) {
        let got: Vec<f64> = fields.iter().map(|f| f.arrows.center().1).collect();
        assert_eq!(got.len(), expected.len(), "{got:?}");
        for (g, e) in got.iter().zip(expected) {
            assert!((g - e).abs() <= 1.0, "{got:?} != {expected:?}");
        }
    }

    #[test]
    fn finds_the_three_rows_of_minotaur() {
        let f = real("real_hx_dist");
        assert_rows(&f, &[388.5, 440.0, 491.5]);
        // "4.2" の文字の真ん中 (x 1902〜1924)
        let (vx, vy) = f[0].value.center();
        assert!(
            (vx - 1913.0).abs() <= 2.0 && (vy - 389.0).abs() <= 3.0,
            "{vx}, {vy}"
        );
    }

    #[test]
    fn values_with_units_are_found_whole() {
        let f = real("real_hx_simple_delay");
        assert_rows(&f, &[391.5, 443.5, 494.5, 546.0]);
        // "600 ms" は x 1871〜1925
        assert_eq!((f[0].value.left, f[0].value.right), (1871, 1925));
    }

    #[test]
    fn rows_without_arrows_are_skipped() {
        // Bit Depth (段階のボタン) と Trails (Off/On) は数えない。Sample Rate は「48 kHz ▾」も含めて 1 行
        let f = real("real_hx_vintage_swell");
        assert_eq!(f.len(), 10);
        assert_rows(&f[..3], &[388.5, 440.5, 543.5]);
        assert_eq!((f[2].value.left, f[2].value.right), (1866, 1929));
    }

    #[test]
    fn choice_rows_between_numeric_rows_are_skipped() {
        // Tone Sovereign: Clipping・Gain Mod・Voltage の行は数えず、数値の行は 8 つ
        assert_eq!(real("real_hx_tone_sovereign").len(), 8);
    }

    #[test]
    fn nothing_is_found_while_a_dialog_dims_the_screen() {
        // 星マークを押して「Save As Favorite」が開いた画面。背景が暗くなり数値の色が消えるので、
        // 行が見つからず、打ち込まない
        assert!(real("real_hx_favorite_dialog").is_empty());
    }

    #[test]
    fn the_mock_screen_used_by_the_ai_tests_has_seven_rows() {
        // scripts/make_mock_screenshot.swift の画面 (2 倍)。Treble は 4 番目で、高さ 194 + 25.7 * 3 ポイント
        let path = format!(
            "{}/tests/fixtures/mock_hx_edit.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let f = find(&image::open(&path).unwrap().to_rgba8(), 2.0);
        assert_eq!(f.len(), 7);
        let (vx, vy) = f[3].value.center();
        assert!((vx / 2.0 - 956.0).abs() <= 3.0, "{vx}");
        assert!((vy / 2.0 - (194.0 + 25.7 * 3.0)).abs() <= 3.0, "{vy}");
    }

    #[test]
    fn nearest_row_by_height() {
        let f = real("real_hx_dist");
        assert_eq!(nearest(&f, 400.0).unwrap().0, 1);
        assert_eq!(nearest(&f, 460.0).unwrap().0, 2);
        assert_eq!(nearest(&f, 470.0).unwrap().0, 3, "492 の方が近い");
        assert_eq!(nearest(&f, 900.0).unwrap().0, 3);
        assert!(nearest(&[], 400.0).is_none());
    }
}
