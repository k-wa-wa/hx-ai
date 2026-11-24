//! HX Edit のプリセットの保存・書き出し・読み込みの流れ。画面の操作は `Ui` を通す。
//!
//! - 保存: File > Save Preset
//! - 書き出し: File > Export Preset... → 標準の保存の画面で、Cmd+Shift+G でフォルダを入れ、名前を入れて Save
//! - 読み込み: `open` で .hlx を HX Edit に渡す → 上書きの確認 (文にファイルのパスが出る) で Yes

// サイクルに組み込むまで (このあとの変更で使う)、使われない警告を許す
#![allow(dead_code)]

use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use crate::ax_tree::{self, Node, Path};

/// HX Edit の画面の操作。本物は macOS のアクセシビリティ、テストでは偽物。
pub trait Ui {
    /// アプリの画面の作り。根の子は [メニューバー, ウィンドウ...]。
    fn snapshot(&mut self) -> Result<Node, String>;
    /// 直前の `snapshot` の `path` の部品を押す。
    fn press(&mut self, path: &[usize]) -> Result<(), String>;
    /// 直前の `snapshot` の `path` の入力欄に文字を入れる。
    fn set_value(&mut self, path: &[usize], value: &str) -> Result<(), String>;
    /// 入力欄の確定 (Return と同じ)。
    fn confirm(&mut self, path: &[usize]) -> Result<(), String>;
    /// 保存の画面で「フォルダへ移動」を開く (Cmd+Shift+G)。
    fn open_go_to_folder(&mut self) -> Result<(), String>;
    /// .hlx を HX Edit で開く。
    fn open_in_hx_edit(&mut self, file: &FsPath) -> Result<(), String>;
    fn file_exists(&self, file: &FsPath) -> bool;
    fn wait(&mut self, d: Duration);
}

pub const POLL: Duration = Duration::from_millis(100);
/// メニューを押してから画面が開くまでの上限。
pub const PANEL_TIMEOUT: Duration = Duration::from_secs(5);
/// 読み込みの確認の Yes を押してから、HX Edit が HX Stomp に送り終えるまで待つ時間。
/// 実機では 1 秒ほどかかった。
pub const AFTER_IMPORT: Duration = Duration::from_millis(1500);
/// 保存を押してから、ダイアログが出ていないかを見るまでの時間。
pub const AFTER_SAVE: Duration = Duration::from_millis(500);

/// `check` が Some を返すまで、`POLL` ごとに撮り直す。
fn wait_for<T>(
    ui: &mut dyn Ui,
    timeout: Duration,
    what: &str,
    check: impl Fn(&Node) -> Option<T>,
) -> Result<T, String> {
    let tries = (timeout.as_millis() / POLL.as_millis()).max(1);
    let mut last = None;
    for _ in 0..=tries {
        let root = ui.snapshot()?;
        if let Some(t) = check(&root) {
            return Ok(t);
        }
        last = Some(root);
        ui.wait(POLL);
    }
    let seen = last
        .map(|r| ax_tree::describe_dialogs(&r))
        .unwrap_or_default();
    Err(format!("{what} (画面: {seen})"))
}

/// File メニューの項目を押す。保存の直後などはしばらく押せないので、押せるようになるまで待つ。
fn press_menu(ui: &mut dyn Ui, item: &str) -> Result<(), String> {
    let root = ui.snapshot()?;
    if ax_tree::menu_item(&root, "File", item).is_none() {
        return Err(format!("HX Edit のメニュー File > {item} が見つかりません"));
    }
    let path = wait_for(
        ui,
        PANEL_TIMEOUT,
        &format!("HX Edit のメニュー File > {item} が押せません (HX Stomp がつながっていない恐れ)"),
        |r| ax_tree::menu_item(r, "File", item).filter(|p| r.get(p).is_some_and(|n| n.enabled)),
    )?;
    ui.press(&path)
}

/// 今のプリセットを HX Stomp のスロットに保存する。書き出しは保存済みの値になるので、先に保存する。
pub fn save(ui: &mut dyn Ui) -> Result<(), String> {
    press_menu(ui, "Save Preset")?;
    ui.wait(AFTER_SAVE);
    let root = ui.snapshot()?;
    if !ax_tree::dialogs(&root).is_empty() {
        return Err(format!(
            "保存したら想定していないダイアログが出ました。触らずに止めます ({})",
            ax_tree::describe_dialogs(&root)
        ));
    }
    Ok(())
}

/// 今のプリセットを `dir/name.hlx` に書き出し、そのパスを返す。
/// 途中で失敗したら、開いた画面を閉じてからエラーを返す。
pub fn export(ui: &mut dyn Ui, dir: &FsPath, name: &str) -> Result<PathBuf, String> {
    let dir_text = dir
        .to_str()
        .ok_or("書き出し先のフォルダの名前を文字にできません")?;
    let folder = dir
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or("書き出し先のフォルダの名前がありません")?;
    let file = dir.join(format!("{name}.hlx"));
    if ui.file_exists(&file) {
        return Err(format!(
            "書き出し先に同じ名前のファイルがあります ({})",
            file.display()
        ));
    }
    press_menu(ui, "Export Preset...")?;
    let result = export_in_panel(ui, dir_text, folder, name, &file);
    if result.is_err() {
        close_panels(ui);
    }
    result.map(|_| file)
}

fn export_in_panel(
    ui: &mut dyn Ui,
    dir: &str,
    folder: &str,
    name: &str,
    file: &FsPath,
) -> Result<(), String> {
    wait_for(
        ui,
        PANEL_TIMEOUT,
        "書き出しの画面が開きません",
        |r| ax_tree::by_id(r, "save-panel"),
    )?;

    // フォルダ
    ui.open_go_to_folder()?;
    let field = wait_for(
        ui,
        PANEL_TIMEOUT,
        "「フォルダへ移動」の入力欄が開きません",
        |r| ax_tree::by_id(r, "PathTextField"),
    )?;
    ui.set_value(&field, dir)?;
    ui.confirm(&field)?;
    wait_for(
        ui,
        PANEL_TIMEOUT,
        "「フォルダへ移動」が閉じません",
        |r| ax_tree::by_id(r, "GoToWindow").is_none().then_some(()),
    )?;
    let root = ui.snapshot()?;
    let place = ax_tree::by_id(&root, "where popup")
        .and_then(|p| root.get(&p))
        .map(|n| n.value.clone())
        .unwrap_or_default();
    if place != folder {
        return Err(format!(
            "書き出し先のフォルダを選べませんでした (「{place}」になっている。{folder} のはず)"
        ));
    }

    // 名前
    let name_field = ax_tree::by_id(&root, "saveAsNameTextField")
        .ok_or("書き出しの画面に名前の欄がありません")?;
    ui.set_value(&name_field, name)?;
    let root = ui.snapshot()?;
    let typed = ax_tree::by_id(&root, "saveAsNameTextField")
        .and_then(|p| root.get(&p))
        .map(|n| n.value.clone())
        .unwrap_or_default();
    if typed != name && typed != format!("{name}.hlx") {
        return Err(format!(
            "書き出しの名前を入れられませんでした (「{typed}」になっている)"
        ));
    }

    // 保存
    let ok = ax_tree::by_id(&root, "OKButton").ok_or("書き出しの画面に Save がありません")?;
    if !root.get(&ok).is_some_and(|n| n.enabled) {
        return Err("書き出しの画面の Save が押せません".into());
    }
    ui.press(&ok)?;
    wait_for(
        ui,
        PANEL_TIMEOUT,
        "書き出しの画面が閉じません",
        |r| ax_tree::dialogs(r).is_empty().then_some(()),
    )?;
    // ファイルができるまで待つ
    for _ in 0..=(PANEL_TIMEOUT.as_millis() / POLL.as_millis()) {
        if ui.file_exists(file) {
            return Ok(());
        }
        ui.wait(POLL);
    }
    Err(format!(
        "書き出したファイルが見つかりません ({})",
        file.display()
    ))
}

/// 開いている書き出しの画面を閉じる (失敗の後片付け。ここでの失敗は無視する)。
fn close_panels(ui: &mut dyn Ui) {
    for (sheet, button) in [
        ("GoToWindow", "CloseButton"),
        ("save-panel", "CancelButton"),
    ] {
        let Ok(root) = ui.snapshot() else { return };
        let Some(s) = ax_tree::by_id(&root, sheet) else {
            continue;
        };
        let Some(node) = root.get(&s) else { continue };
        if let Some(mut b) = ax_tree::by_id(node, button) {
            let mut full = s.clone();
            full.append(&mut b);
            let _ = ui.press(&full);
            ui.wait(POLL);
        }
    }
}

/// `file` を今のプリセットに読み込む (HX Stomp の選んでいるスロットを上書きする)。
pub fn import(ui: &mut dyn Ui, file: &FsPath) -> Result<(), String> {
    let text = file
        .to_str()
        .ok_or("読み込むファイルの名前を文字にできません")?;
    let root = ui.snapshot()?;
    if !ax_tree::dialogs(&root).is_empty() {
        return Err(format!(
            "HX Edit にダイアログが開いているので読み込みません ({})",
            ax_tree::describe_dialogs(&root)
        ));
    }
    ui.open_in_hx_edit(file)?;
    let yes: Path = wait_for(
        ui,
        PANEL_TIMEOUT,
        "読み込みの確認が出ません",
        |r| ax_tree::overwrite_yes(r, text),
    )?;
    ui.press(&yes)?;
    wait_for(
        ui,
        PANEL_TIMEOUT,
        "読み込みの確認が閉じません",
        |r| ax_tree::dialogs(r).is_empty().then_some(()),
    )?;
    ui.wait(AFTER_IMPORT);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax_tree::build::*;
    use std::collections::HashSet;

    /// 書き出しの画面の状態。
    #[derive(Debug, Clone)]
    struct Panel {
        /// 「場所」に出ているフォルダの名前。
        place: String,
        /// 実際のフォルダ。
        dir: String,
        name: String,
        /// 「フォルダへ移動」の入力欄 (開いているとき)。
        go_to: Option<String>,
    }

    /// 偽の HX Edit。押された部品に応じて画面の状態を変える。
    struct FakeHx {
        panel: Option<Panel>,
        /// 読み込みの確認に出ているパス。
        alert: Option<String>,
        files: HashSet<PathBuf>,
        imported: Vec<String>,
        saved: usize,
        last: Node,
        log: Vec<String>,
        // ---- 実物と違う振る舞いを起こす ----
        /// 「フォルダへ移動」で確定しても、フォルダが変わらない。
        go_to_ignored: bool,
        /// .hlx を開いたときの確認に出すパス (None なら開いたファイルのパス)。
        alert_shows: Option<String>,
        /// .hlx を開いても確認が出ない。
        no_alert: bool,
        /// 保存するとダイアログが出る。
        save_opens_dialog: bool,
        /// 書き出しのメニューが、写し取るこの回数のあいだ押せない
        /// (保存の直後。usize::MAX なら HX Stomp がつながっていない)。
        export_disabled_for: usize,
    }

    impl FakeHx {
        fn new() -> Self {
            Self {
                panel: None,
                alert: None,
                files: HashSet::new(),
                imported: Vec::new(),
                saved: 0,
                last: Node::default(),
                log: Vec::new(),
                go_to_ignored: false,
                alert_shows: None,
                no_alert: false,
                save_opens_dialog: false,
                export_disabled_for: 0,
            }
        }

        fn tree(&self) -> Node {
            let mut root = app(vec![main_window(
                self.panel
                    .iter()
                    .map(|p| save_panel(&p.place, &p.name, p.go_to.as_deref().map(go_to_sheet)))
                    .collect(),
            )]);
            if self.export_disabled_for > 0 {
                let p = ax_tree::menu_item(&root, "File", "Export Preset...").unwrap();
                let (last, parent) = p.split_last().unwrap();
                let mut n = &mut root;
                for i in parent {
                    n = &mut n.children[*i];
                }
                n.children[*last].enabled = false;
            }
            if let Some(f) = &self.alert {
                root.children.push(overwrite_alert(f));
            }
            root
        }

        fn node(&self, path: &[usize]) -> Node {
            self.last.get(path).expect("撮った画面に無い部品").clone()
        }
    }

    const DIR: &str = "/Users/me/Library/Application Support/hx-ai/presets";

    impl Ui for FakeHx {
        fn snapshot(&mut self) -> Result<Node, String> {
            self.last = self.tree();
            self.export_disabled_for = self.export_disabled_for.saturating_sub(1);
            Ok(self.last.clone())
        }
        fn press(&mut self, path: &[usize]) -> Result<(), String> {
            let n = self.node(path);
            let label = if n.title.is_empty() {
                n.id.clone()
            } else {
                n.title.clone()
            };
            self.log.push(format!("押す {label}"));
            match (n.role.as_str(), label.as_str()) {
                ("AXMenuItem", "Export Preset...") => {
                    self.panel = Some(Panel {
                        place: "Downloads".into(),
                        dir: "/Users/me/Downloads".into(),
                        name: "New Preset".into(),
                        go_to: None,
                    })
                }
                ("AXMenuItem", "Save Preset") => {
                    self.saved += 1;
                    if self.save_opens_dialog {
                        self.alert = Some("保存の確認".into());
                    }
                }
                ("AXButton", "Save") => {
                    let p = self.panel.take().unwrap();
                    self.files
                        .insert(PathBuf::from(format!("{}/{}.hlx", p.dir, p.name)));
                }
                ("AXButton", "Cancel") => self.panel = None,
                ("AXButton", "CloseButton") => self.panel.as_mut().unwrap().go_to = None,
                ("AXButton", "Yes") => self.imported.push(self.alert.take().unwrap()),
                ("AXButton", "No") => self.alert = None,
                other => panic!("想定していない部品を押した: {other:?}"),
            }
            Ok(())
        }
        fn set_value(&mut self, path: &[usize], value: &str) -> Result<(), String> {
            let n = self.node(path);
            self.log.push(format!("入れる {} = {value}", n.id));
            let p = self.panel.as_mut().unwrap();
            match n.id.as_str() {
                "PathTextField" => p.go_to = Some(value.into()),
                "saveAsNameTextField" => p.name = value.into(),
                other => panic!("想定していない入力欄: {other}"),
            }
            Ok(())
        }
        fn confirm(&mut self, path: &[usize]) -> Result<(), String> {
            let n = self.node(path);
            self.log.push(format!("確定 {}", n.id));
            let p = self.panel.as_mut().unwrap();
            let dir = p.go_to.take().unwrap();
            if !self.go_to_ignored {
                p.place = dir.rsplit('/').next().unwrap().into();
                p.dir = dir;
            }
            Ok(())
        }
        fn open_go_to_folder(&mut self) -> Result<(), String> {
            self.log.push("Cmd+Shift+G".into());
            if let Some(p) = self.panel.as_mut() {
                p.go_to = Some(String::new());
            }
            Ok(())
        }
        fn open_in_hx_edit(&mut self, file: &FsPath) -> Result<(), String> {
            self.log.push(format!("開く {}", file.display()));
            if !self.no_alert {
                self.alert = Some(
                    self.alert_shows
                        .clone()
                        .unwrap_or_else(|| file.display().to_string()),
                );
            }
            Ok(())
        }
        fn file_exists(&self, file: &FsPath) -> bool {
            self.files.contains(file)
        }
        fn wait(&mut self, _: Duration) {}
    }

    // ---- 書き出し ----

    #[test]
    fn export_goes_to_the_folder_names_the_file_and_saves() {
        let mut hx = FakeHx::new();
        let file = export(&mut hx, FsPath::new(DIR), "hx-ai-1").unwrap();
        assert_eq!(file, PathBuf::from(format!("{DIR}/hx-ai-1.hlx")));
        assert!(hx.files.contains(&file));
        assert_eq!(
            hx.log,
            [
                "押す Export Preset...".to_string(),
                "Cmd+Shift+G".into(),
                format!("入れる PathTextField = {DIR}"),
                "確定 PathTextField".into(),
                "入れる saveAsNameTextField = hx-ai-1".into(),
                "押す Save".into(),
            ]
        );
        assert!(hx.panel.is_none());
    }

    #[test]
    fn export_stops_and_cancels_when_the_folder_did_not_change() {
        let mut hx = FakeHx::new();
        hx.go_to_ignored = true;
        let e = export(&mut hx, FsPath::new(DIR), "hx-ai-1").unwrap_err();
        assert!(e.contains("「Downloads」") && e.contains("presets"), "{e}");
        assert!(hx.files.is_empty(), "違う場所に書き出さない");
        assert!(hx.panel.is_none(), "画面を閉じる");
        assert_eq!(hx.log.last().unwrap(), "押す Cancel");
    }

    #[test]
    fn export_refuses_to_overwrite_an_existing_file() {
        let mut hx = FakeHx::new();
        hx.files.insert(PathBuf::from(format!("{DIR}/hx-ai-1.hlx")));
        let e = export(&mut hx, FsPath::new(DIR), "hx-ai-1").unwrap_err();
        assert!(e.contains("同じ名前"), "{e}");
        assert!(hx.log.is_empty(), "メニューも押さない");
    }

    #[test]
    fn export_needs_the_menu_to_be_enabled() {
        let mut hx = FakeHx::new();
        hx.export_disabled_for = usize::MAX;
        let e = export(&mut hx, FsPath::new(DIR), "hx-ai-1").unwrap_err();
        assert!(e.contains("押せません"), "{e}");
        assert!(hx.log.is_empty());
    }

    #[test]
    fn export_waits_for_the_menu_that_is_briefly_disabled_after_saving() {
        // 実機では、保存の直後に Export Preset... が押せない状態だった
        let mut hx = FakeHx::new();
        hx.export_disabled_for = 3;
        export(&mut hx, FsPath::new(DIR), "hx-ai-1").unwrap();
        assert_eq!(hx.log[0], "押す Export Preset...");
    }

    // ---- 読み込み ----

    #[test]
    fn import_opens_the_file_and_presses_yes_for_it() {
        let mut hx = FakeHx::new();
        let file = format!("{DIR}/hx-ai-2.hlx");
        import(&mut hx, FsPath::new(&file)).unwrap();
        assert_eq!(hx.imported, std::slice::from_ref(&file));
        assert_eq!(hx.log, [format!("開く {file}"), "押す Yes".to_string()]);
    }

    #[test]
    fn import_does_not_press_yes_for_another_file() {
        let mut hx = FakeHx::new();
        hx.alert_shows = Some("/Users/me/Downloads/other.hlx".into());
        let e = import(&mut hx, FsPath::new(&format!("{DIR}/hx-ai-2.hlx"))).unwrap_err();
        assert!(
            e.contains("確認が出ません") && e.contains("other.hlx"),
            "{e}"
        );
        assert!(hx.imported.is_empty());
        assert!(hx.alert.is_some(), "人間に任せるため閉じない");
    }

    #[test]
    fn import_fails_when_no_confirmation_appears() {
        let mut hx = FakeHx::new();
        hx.no_alert = true;
        let e = import(&mut hx, FsPath::new(&format!("{DIR}/hx-ai-2.hlx"))).unwrap_err();
        assert!(e.contains("確認が出ません"), "{e}");
    }

    #[test]
    fn import_refuses_while_a_dialog_is_open() {
        let mut hx = FakeHx::new();
        hx.alert = Some("/Users/me/Downloads/other.hlx".into());
        let e = import(&mut hx, FsPath::new(&format!("{DIR}/hx-ai-2.hlx"))).unwrap_err();
        assert!(e.contains("ダイアログが開いている"), "{e}");
        assert!(hx.log.is_empty(), "開かない");
    }

    // ---- 保存 ----

    #[test]
    fn save_presses_save_preset() {
        let mut hx = FakeHx::new();
        save(&mut hx).unwrap();
        assert_eq!(hx.saved, 1);
        assert_eq!(hx.log, ["押す Save Preset"]);
    }

    #[test]
    fn save_stops_when_a_dialog_appears() {
        let mut hx = FakeHx::new();
        hx.save_opens_dialog = true;
        let e = save(&mut hx).unwrap_err();
        assert!(e.contains("触らずに止めます"), "{e}");
        assert!(hx.alert.is_some(), "ダイアログに触らない");
    }

    #[test]
    fn timings() {
        assert_eq!(POLL, Duration::from_millis(100));
        assert_eq!(PANEL_TIMEOUT, Duration::from_secs(5));
        assert_eq!(AFTER_IMPORT, Duration::from_millis(1500));
    }
}
