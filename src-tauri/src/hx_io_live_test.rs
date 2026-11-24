//! 実機の HX Edit と HX Stomp で、書き出し → 書き換え → 読み込み → 書き出し の往復を確かめる。
//!
//! 選んでいるスロットを上書きする (最後に元に戻す)。空いているスロットを選んでから、手で走らせる:
//!
//! ```sh
//! cd src-tauri && HX_EDIT_LIVE=1 cargo test hx_edit_round_trip -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::ax::MacUi;
use crate::hx_io;
use crate::preset::{self, Change, Defs, Kind};

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn step<T>(name: &str, f: impl FnOnce() -> Result<T, String>) -> T {
    let start = Instant::now();
    match f() {
        Ok(v) => {
            println!("{name}: {:.1} 秒", start.elapsed().as_secs_f64());
            v
        }
        Err(e) => panic!("{name} で失敗: {e}"),
    }
}

/// 最初のブロックの、変えられる最初の数値のパラメータを 1 だけ動かす変更。
fn nudge(blocks: &[preset::BlockView]) -> Change {
    for b in blocks {
        for p in &b.params {
            if let (Kind::Number { min, max, .. }, false) = (&p.kind, p.locked) {
                let v: f64 = p.value.parse().unwrap();
                let next = if v + 1.0 <= *max { v + 1.0 } else { v - 1.0 };
                assert!(next >= *min);
                return Change {
                    block: b.id.clone(),
                    parameter: p.name.clone(),
                    value: format!("{next}"),
                };
            }
        }
    }
    panic!("動かせる数値のパラメータがありません。ブロックのあるプリセットを選んでください");
}

#[test]
#[ignore]
fn hx_edit_round_trip() {
    assert_eq!(
        std::env::var("HX_EDIT_LIVE").as_deref(),
        Ok("1"),
        "選んでいるスロットを上書きするので、HX_EDIT_LIVE=1 を付けたときだけ走らせる"
    );
    let defs = Defs::load(Path::new(preset::HX_EDIT_RESOURCES)).unwrap();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let dir: PathBuf = std::env::temp_dir().join(format!("hx-ai-live-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    println!("書き出し先: {}", dir.display());
    let mut ui = MacUi::connect().unwrap();

    // 保存 (Save Preset) の直後は、手で操作しても書き出しが効かない時間があった。
    // 切り分けるため、ここでは保存せず、書き出しと読み込みの往復だけを確かめる
    let original = step("書き出し (元)", || {
        hx_io::export(&mut ui, &dir, "original")
    });
    let before = read(&original);
    let blocks = preset::describe(&before, &defs).unwrap();
    print!("{}", preset::render(&blocks));

    let change = nudge(&blocks);
    let (changed, applied) = preset::apply(&before, &defs, &[change]).unwrap();
    println!("変える: {applied:?}");
    let changed_file = dir.join("changed.hlx");
    std::fs::write(
        &changed_file,
        serde_json::to_string_pretty(&changed).unwrap(),
    )
    .unwrap();

    step("読み込み (変えたもの)", || {
        hx_io::import(&mut ui, &changed_file)
    });
    let after_file = step("書き出し (読み込んだ後)", || {
        hx_io::export(&mut ui, &dir, "after")
    });
    let after = read(&after_file);
    assert_eq!(
        preset::render(&preset::describe(&after, &defs).unwrap()),
        preset::render(&preset::describe(&changed, &defs).unwrap()),
        "読み込んだ値が HX Stomp に入っていない"
    );

    step("読み込み (元に戻す)", || {
        hx_io::import(&mut ui, &original)
    });
    let restored_file = step("書き出し (戻した後)", || {
        hx_io::export(&mut ui, &dir, "restored")
    });
    assert_eq!(
        preset::render(&preset::describe(&read(&restored_file), &defs).unwrap()),
        preset::render(&blocks),
        "元に戻っていない"
    );
    println!("往復できた");
}
