//! ダブルクリックの前後の画面を比べ、数値欄の入力が始まったかを判断する。OS には触らない。
//!
//! AI の座標がずれると、数値欄ではなく星マーク (Favorites に追加) やモデル名を押して
//! ダイアログや一覧が開く。そこに値を打ち込まないよう、打ち込む前に確かめる。

use image::RgbaImage;

/// クリックした点の「周り」とみなす範囲 (ポイント、中心からの半分の幅と高さ)。
/// 数値欄が入力状態になったときの見た目の変化と、マウスを乗せたときの強調が収まる大きさ。
pub const NEAR_HALF_WIDTH: f64 = 160.0;
pub const NEAR_HALF_HEIGHT: f64 = 30.0;
/// 周り以外で変わった画素が、ウィンドウ全体のこの割合を超えたら、別のものが開いたとみなす。
pub const MAX_FAR_CHANGE: f64 = 0.002;
/// 周りで変わった画素がこの数より少なければ、入力欄が開いていないとみなす。
pub const MIN_NEAR_CHANGE: usize = 20;
/// 変わったとみなす各チャンネルの差。
const CHANNEL_TOLERANCE: i32 = 24;

/// 変わった画素の数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diff {
    /// クリックした点の周り。
    pub near: usize,
    /// それ以外。
    pub far: usize,
    /// 画像全体の画素数。
    pub total: usize,
}

/// 前後の画像を比べる。`center` と `half` は画像のピクセルで、周りの範囲を表す。
pub fn diff(
    before: &RgbaImage,
    after: &RgbaImage,
    center: (u32, u32),
    half: (u32, u32),
) -> Result<Diff, String> {
    if before.dimensions() != after.dimensions() {
        return Err(format!(
            "ダブルクリックの前後でウィンドウの大きさが変わりました ({:?} → {:?})",
            before.dimensions(),
            after.dimensions()
        ));
    }
    let near_x = center.0.saturating_sub(half.0)..=center.0 + half.0;
    let near_y = center.1.saturating_sub(half.1)..=center.1 + half.1;
    let mut d = Diff {
        near: 0,
        far: 0,
        total: (before.width() * before.height()) as usize,
    };
    for (x, y, a) in before.enumerate_pixels() {
        let b = after.get_pixel(x, y);
        let changed =
            a.0.iter()
                .zip(b.0.iter())
                .take(3)
                .any(|(p, q)| (*p as i32 - *q as i32).abs() > CHANNEL_TOLERANCE);
        if changed {
            if near_x.contains(&x) && near_y.contains(&y) {
                d.near += 1;
            } else {
                d.far += 1;
            }
        }
    }
    Ok(d)
}

/// 打ち込んではいけない理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// 別のもの (ダイアログ・一覧) が開いた。Escape で閉じる。
    Opened(String),
    /// 何も開かなかった。
    Nothing(String),
}

impl Refusal {
    pub fn message(&self) -> &str {
        match self {
            Self::Opened(m) | Self::Nothing(m) => m,
        }
    }
}

/// 打ち込んでよいかを決める。
pub fn check_double_click(d: &Diff) -> Result<(), Refusal> {
    if d.far as f64 > d.total as f64 * MAX_FAR_CHANGE {
        return Err(Refusal::Opened(format!(
            "ダブルクリックした所の周り以外の画面が大きく変わりました ({:.1}%。ダイアログや一覧が開いた恐れ)。数値欄を押せていないので、打ち込まずに Escape を押して止めました",
            d.far as f64 / d.total as f64 * 100.0
        )));
    }
    if d.near < MIN_NEAR_CHANGE {
        return Err(Refusal::Nothing(
            "ダブルクリックしても数値の入力欄が開きませんでした (画面が変わらない)。数値欄を押せていないので、打ち込まずに止めました"
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    const BG: Rgba<u8> = Rgba([30, 30, 34, 255]);

    fn canvas() -> RgbaImage {
        RgbaImage::from_pixel(1000, 600, BG)
    }

    fn paint(img: &mut RgbaImage, x: std::ops::Range<u32>, y: std::ops::Range<u32>, c: [u8; 3]) {
        for yy in y {
            for xx in x.clone() {
                img.put_pixel(xx, yy, Rgba([c[0], c[1], c[2], 255]));
            }
        }
    }

    #[test]
    fn counts_changes_near_and_far_from_the_click() {
        let before = canvas();
        let mut after = canvas();
        paint(&mut after, 480..520, 290..310, [200, 200, 200]); // 入力欄 (周り)
        paint(&mut after, 0..10, 0..10, [200, 200, 200]); // 遠く
        let d = diff(&before, &after, (500, 300), (100, 20)).unwrap();
        assert_eq!(
            d,
            Diff {
                near: 800,
                far: 100,
                total: 600_000
            }
        );
    }

    #[test]
    fn the_near_box_edges_count_as_near() {
        let before = canvas();
        let mut after = canvas();
        after.put_pixel(400, 280, Rgba([255, 255, 255, 255]));
        after.put_pixel(600, 320, Rgba([255, 255, 255, 255]));
        after.put_pixel(399, 300, Rgba([255, 255, 255, 255]));
        after.put_pixel(500, 321, Rgba([255, 255, 255, 255]));
        let d = diff(&before, &after, (500, 300), (100, 20)).unwrap();
        assert_eq!((d.near, d.far), (2, 2));
    }

    #[test]
    fn small_color_shifts_are_not_changes() {
        let before = canvas();
        let mut after = canvas();
        paint(&mut after, 0..1000, 0..600, [30 + 24, 30, 34 - 24]);
        assert_eq!(diff(&before, &after, (500, 300), (100, 20)).unwrap().far, 0);
        paint(&mut after, 0..1, 0..1, [30 + 25, 30, 34]);
        assert_eq!(diff(&before, &after, (500, 300), (100, 20)).unwrap().far, 1);
    }

    #[test]
    fn near_box_is_clamped_at_the_image_edge() {
        let before = canvas();
        let mut after = canvas();
        paint(&mut after, 0..5, 0..5, [200, 200, 200]);
        let d = diff(&before, &after, (2, 2), (100, 20)).unwrap();
        assert_eq!((d.near, d.far), (25, 0));
    }

    #[test]
    fn different_sizes_are_an_error() {
        let a = canvas();
        let b = RgbaImage::from_pixel(1000, 601, BG);
        assert!(diff(&a, &b, (0, 0), (1, 1)).unwrap_err().contains("大きさ"));
    }

    #[test]
    fn a_field_that_opened_in_place_is_ok() {
        let d = Diff {
            near: MIN_NEAR_CHANGE,
            far: 1200,
            total: 600_000,
        };
        assert!(
            check_double_click(&d).is_ok(),
            "遠くの小さな変化 (0.2%) までは許す"
        );
    }

    #[test]
    fn a_dialog_or_list_that_opened_is_refused() {
        let d = Diff {
            near: 500,
            far: 1201,
            total: 600_000,
        };
        let Err(Refusal::Opened(e)) = check_double_click(&d) else {
            panic!("開いたものを閉じる理由になっていない")
        };
        assert!(e.contains("Escape") && e.contains("0.2%"), "{e}");
    }

    #[test]
    fn nothing_opening_is_refused() {
        let d = Diff {
            near: MIN_NEAR_CHANGE - 1,
            far: 0,
            total: 600_000,
        };
        let Err(Refusal::Nothing(e)) = check_double_click(&d) else {
            panic!("何も開かなかった理由になっていない")
        };
        assert!(e.contains("開きませんでした"));
    }

    #[test]
    fn thresholds() {
        assert_eq!((NEAR_HALF_WIDTH, NEAR_HALF_HEIGHT), (160.0, 30.0));
        assert_eq!(MAX_FAR_CHANGE, 0.002);
        assert_eq!(MIN_NEAR_CHANGE, 20);
    }
}
