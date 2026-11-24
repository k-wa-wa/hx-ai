//! アクセシビリティで写し取った画面の作り (木) と、そこから部品を探す関数。OS には触らない。
//!
//! 木の根はアプリで、子は [メニューバー, ウィンドウ 1, ウィンドウ 2, ...] の順に並ぶ。
//! 部品の場所は、根からたどる子の番号の並び (`Path`) で表す。

// 書き出し・読み込みの流れに組み込むまで (このあとの変更で使う)、使われない警告を許す
#![allow(dead_code)]

/// 画面の部品 1 つ。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub role: String,
    pub subrole: String,
    pub title: String,
    /// AXIdentifier。標準のファイルの画面では決まった名前が付いている (`save-panel` など)。
    pub id: String,
    pub desc: String,
    pub value: String,
    pub enabled: bool,
    pub children: Vec<Node>,
}

pub type Path = Vec<usize>;

impl Node {
    pub fn get(&self, path: &[usize]) -> Option<&Node> {
        path.iter().try_fold(self, |n, &i| n.children.get(i))
    }
}

/// 条件に合う最初の部品 (深さ優先)。
pub fn find(root: &Node, pred: &dyn Fn(&Node) -> bool) -> Option<Path> {
    if pred(root) {
        return Some(Vec::new());
    }
    for (i, c) in root.children.iter().enumerate() {
        if let Some(mut p) = find(c, pred) {
            p.insert(0, i);
            return Some(p);
        }
    }
    None
}

pub fn by_id(root: &Node, id: &str) -> Option<Path> {
    find(root, &|n| n.id == id)
}

/// メニューバーの `menu` の中の `item`。
pub fn menu_item(root: &Node, menu: &str, item: &str) -> Option<Path> {
    let (bar_i, bar) = root
        .children
        .iter()
        .enumerate()
        .find(|(_, n)| n.role == "AXMenuBar")?;
    let (top_i, top) = bar
        .children
        .iter()
        .enumerate()
        .find(|(_, n)| n.role == "AXMenuBarItem" && n.title == menu)?;
    for (menu_i, m) in top.children.iter().enumerate() {
        if let Some((item_i, _)) = m
            .children
            .iter()
            .enumerate()
            .find(|(_, n)| n.role == "AXMenuItem" && n.title == item)
        {
            return Some(vec![bar_i, top_i, menu_i, item_i]);
        }
    }
    None
}

/// ダイアログ (シートか、ダイアログのウィンドウ) かどうか。
fn is_dialog(n: &Node) -> bool {
    n.role == "AXSheet"
        || (n.role == "AXWindow" && (n.subrole == "AXDialog" || n.subrole == "AXSystemDialog"))
}

/// 開いているダイアログの場所 (外側から順)。
pub fn dialogs(root: &Node) -> Vec<Path> {
    fn walk(n: &Node, path: &mut Path, out: &mut Vec<Path>) {
        if is_dialog(n) {
            out.push(path.clone());
        }
        for (i, c) in n.children.iter().enumerate() {
            path.push(i);
            walk(c, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(root, &mut Vec::new(), &mut out);
    out
}

fn texts(n: &Node, out: &mut Vec<String>) {
    if n.role == "AXStaticText" || n.role == "AXTextArea" {
        for t in [&n.value, &n.title] {
            if !t.is_empty() {
                out.push(t.clone());
            }
        }
    }
    for c in &n.children {
        texts(c, out);
    }
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 「このファイルで今のプリセットを上書きするか」の確認の、Yes のボタン。
/// ダイアログの文に `file` のパスが書かれているものだけを選ぶ (折り返しの空白は無視する)。
pub fn overwrite_yes(root: &Node, file: &str) -> Option<Path> {
    let want = squash(file);
    for d in dialogs(root) {
        let node = root.get(&d)?;
        let mut t = Vec::new();
        texts(node, &mut t);
        if !t.iter().any(|t| squash(t).contains(&want)) {
            continue;
        }
        if let Some(mut p) = find(node, &|n| n.role == "AXButton" && n.title == "Yes") {
            let mut full = d.clone();
            full.append(&mut p);
            return Some(full);
        }
    }
    None
}

/// 開いているダイアログの説明 (エラーの文に添える)。
pub fn describe_dialogs(root: &Node) -> String {
    let mut parts = Vec::new();
    for d in dialogs(root) {
        let Some(n) = root.get(&d) else { continue };
        let mut t = Vec::new();
        texts(n, &mut t);
        let name = [&n.id, &n.desc, &n.title]
            .into_iter()
            .find(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| n.role.clone());
        let joined = t.join(" / ");
        parts.push(if joined.is_empty() {
            name
        } else {
            format!("{name}: {joined}")
        });
    }
    if parts.is_empty() {
        "ダイアログは開いていない".into()
    } else {
        parts.join("、")
    }
}

#[cfg(test)]
pub mod build {
    //! テスト用の木を短く書くための関数。
    use super::Node;

    pub fn n(role: &str) -> Node {
        Node {
            role: role.into(),
            enabled: true,
            ..Node::default()
        }
    }

    impl Node {
        pub fn id(mut self, id: &str) -> Self {
            self.id = id.into();
            self
        }
        pub fn title(mut self, t: &str) -> Self {
            self.title = t.into();
            self
        }
        pub fn value(mut self, v: &str) -> Self {
            self.value = v.into();
            self
        }
        pub fn sub(mut self, s: &str) -> Self {
            self.subrole = s.into();
            self
        }
        pub fn desc(mut self, d: &str) -> Self {
            self.desc = d.into();
            self
        }
        pub fn disabled(mut self) -> Self {
            self.enabled = false;
            self
        }
        pub fn with(mut self, children: Vec<Node>) -> Self {
            self.children = children;
            self
        }
    }

    /// HX Edit 3.82 のメニューバー (File の一部だけ)。
    pub fn menu_bar() -> Node {
        n("AXMenuBar").with(vec![
            n("AXMenuBarItem").title("Apple"),
            n("AXMenuBarItem").title("HX Edit"),
            n("AXMenuBarItem")
                .title("File")
                .with(vec![n("AXMenu").with(vec![
                    n("AXMenuItem").title("Save Preset"),
                    n("AXMenuItem").title("Save Preset As..."),
                    n("AXMenuItem"),
                    n("AXMenuItem").title("Import Preset..."),
                    n("AXMenuItem").title("Export Preset..."),
                ])]),
            n("AXMenuBarItem").title("Edit"),
        ])
    }

    /// 本体のウィンドウ。中身は描画領域 1 つだけ。
    pub fn main_window(sheets: Vec<Node>) -> Node {
        let mut c = vec![
            n("AXButton").sub("AXCloseButton"),
            n("AXButton").sub("AXMinimizeButton"),
            n("AXStaticText").value("HX Edit - HX Stomp XL"),
        ];
        c.extend(sheets);
        n("AXWindow")
            .sub("AXStandardWindow")
            .title("HX Edit - HX Stomp XL")
            .with(c)
    }

    /// 書き出しの画面 (実機で写し取った作り)。`go_to` は Cmd+Shift+G で開くシート。
    pub fn save_panel(where_: &str, name: &str, go_to: Option<Node>) -> Node {
        let mut c = vec![n("AXSplitGroup").with(vec![
            n("AXStaticText").id("whereLabel").value("Where:"),
            n("AXPopUpButton")
                .title("Where:")
                .id("where popup")
                .value(where_),
            n("AXStaticText").id("nameFieldLabel").value("Save As:"),
            n("AXTextField").id("saveAsNameTextField").value(name),
            n("AXStaticText").id("tagsLabel").value("Tags:"),
            n("AXTextField").id("_NS:8").desc("tag editor"),
            n("AXButton").title("Cancel").id("CancelButton"),
            n("AXButton").title("Save").id("OKButton"),
        ])];
        c.extend(go_to);
        n("AXSheet")
            .id("save-panel")
            .desc("export preset file")
            .with(c)
    }

    pub fn go_to_sheet(path: &str) -> Node {
        n("AXSheet").id("GoToWindow").with(vec![
            n("AXStaticText").id("_NS:39").value("Go to Folder"),
            n("AXButton").id("CloseButton").desc("Close"),
            n("AXTextField").id("PathTextField").value(path),
        ])
    }

    /// 読み込みの上書きの確認 (`open` で .hlx を開いたとき)。
    pub fn overwrite_alert(file: &str) -> Node {
        n("AXWindow").sub("AXDialog").with(vec![
            n("AXImage"),
            n("AXStaticText").value("HX Edit"),
            n("AXStaticText").value(&format!(
                "Are you sure you want to overwrite the current preset with:\n\n{file}?"
            )),
            n("AXButton").title("No"),
            n("AXButton").title("Yes"),
        ])
    }

    pub fn app(windows: Vec<Node>) -> Node {
        let mut c = vec![menu_bar()];
        c.extend(windows);
        n("AXApplication").title("HX Edit").with(c)
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

    #[test]
    fn menu_items_are_found_by_menu_and_title() {
        let root = app(vec![main_window(vec![])]);
        assert_eq!(
            menu_item(&root, "File", "Export Preset..."),
            Some(vec![0, 2, 0, 4])
        );
        assert_eq!(
            root.get(&menu_item(&root, "File", "Save Preset").unwrap())
                .unwrap()
                .title,
            "Save Preset",
            "「Save Preset As...」と取り違えない"
        );
        assert_eq!(menu_item(&root, "Edit", "Export Preset..."), None);
        assert_eq!(menu_item(&root, "File", "Export"), None);
    }

    #[test]
    fn no_menu_bar_means_no_menu_item() {
        let root = n("AXApplication").with(vec![main_window(vec![])]);
        assert_eq!(menu_item(&root, "File", "Save Preset"), None);
    }

    #[test]
    fn parts_of_the_save_panel_are_found_by_id() {
        let root = app(vec![main_window(vec![save_panel(
            "Downloads",
            "New Preset",
            Some(go_to_sheet("")),
        )])]);
        let field = by_id(&root, "PathTextField").unwrap();
        assert_eq!(field, vec![1, 3, 1, 2]);
        assert_eq!(root.get(&field).unwrap().role, "AXTextField");
        assert_eq!(
            root.get(&by_id(&root, "saveAsNameTextField").unwrap())
                .unwrap()
                .value,
            "New Preset"
        );
        assert_eq!(by_id(&root, "open-panel"), None);
    }

    #[test]
    fn dialogs_are_sheets_and_dialog_windows() {
        let root = app(vec![
            main_window(vec![save_panel("Downloads", "x", Some(go_to_sheet("")))]),
            overwrite_alert("/tmp/a.hlx"),
        ]);
        assert_eq!(dialogs(&root), vec![vec![1, 3], vec![1, 3, 1], vec![2]]);
        assert!(dialogs(&app(vec![main_window(vec![])])).is_empty());
    }

    #[test]
    fn the_yes_button_is_pressed_only_for_our_file() {
        let file = "/Users/me/Library/Application Support/hx-ai/presets/hx-ai-1.hlx";
        let root = app(vec![main_window(vec![]), overwrite_alert(file)]);
        assert_eq!(overwrite_yes(&root, file), Some(vec![2, 4]));
        assert_eq!(
            overwrite_yes(&root, "/Users/me/Downloads/other.hlx"),
            None,
            "別のファイルの確認は押さない"
        );
    }

    #[test]
    fn a_path_wrapped_over_lines_still_matches() {
        // 実機のダイアログは長いパスを折り返して出す
        let mut alert = overwrite_alert("x");
        alert.children[2].value = "Are you sure you want to overwrite the current preset with:\n\n/Users/me/Downloads/\nNew Preset.hlx?".into();
        let root = app(vec![main_window(vec![]), alert]);
        assert!(overwrite_yes(&root, "/Users/me/Downloads/New Preset.hlx").is_some());
    }

    #[test]
    fn a_dialog_without_a_yes_button_is_not_pressed() {
        let mut alert = overwrite_alert("/tmp/a.hlx");
        alert.children[4].title = "OK".into();
        let root = app(vec![main_window(vec![]), alert]);
        assert_eq!(overwrite_yes(&root, "/tmp/a.hlx"), None);
    }

    #[test]
    fn the_path_in_the_main_window_is_not_a_dialog() {
        let mut w = main_window(vec![]);
        w.children.push(n("AXStaticText").value("/tmp/a.hlx"));
        w.children.push(n("AXButton").title("Yes"));
        assert_eq!(overwrite_yes(&app(vec![w]), "/tmp/a.hlx"), None);
    }

    #[test]
    fn open_dialogs_are_described_for_error_messages() {
        let root = app(vec![main_window(vec![]), overwrite_alert("/tmp/a.hlx")]);
        assert_eq!(
            describe_dialogs(&root),
            "AXWindow: HX Edit / Are you sure you want to overwrite the current preset with:\n\n/tmp/a.hlx?"
        );
        let root = app(vec![main_window(vec![save_panel("Downloads", "x", None)])]);
        assert_eq!(
            describe_dialogs(&root),
            "save-panel: Where: / Save As: / Tags:"
        );
        assert_eq!(
            describe_dialogs(&app(vec![main_window(vec![])])),
            "ダイアログは開いていない"
        );
    }
}
