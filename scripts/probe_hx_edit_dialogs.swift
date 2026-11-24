// HX Edit の書き出し・読み込みの画面の作りを、アクセシビリティで写し取る。
// 何も保存しない。開いた画面は Escape で閉じる。
// 使い方 (アクセシビリティを許可したターミナルで、HX Edit を起動して): swift scripts/probe_hx_edit_dialogs.swift > 出力ファイル
import AppKit
import ApplicationServices

func attr(_ e: AXUIElement, _ name: String) -> AnyObject? {
    var v: AnyObject?
    return AXUIElementCopyAttributeValue(e, name as CFString, &v) == .success ? v : nil
}
func str(_ e: AXUIElement, _ name: String) -> String {
    guard let v = attr(e, name) else { return "" }
    if let s = v as? String { return s }
    if let n = v as? NSNumber { return n.stringValue }
    return ""
}
func children(_ e: AXUIElement) -> [AXUIElement] {
    (attr(e, kAXChildrenAttribute) as? [AXUIElement]) ?? []
}
func actions(_ e: AXUIElement) -> [String] {
    var names: CFArray?
    guard AXUIElementCopyActionNames(e, &names) == .success, let n = names as? [String] else { return [] }
    return n
}
func dump(_ e: AXUIElement, _ depth: Int, _ maxDepth: Int) {
    let pad = String(repeating: "  ", count: depth)
    var line = "\(pad)\(str(e, kAXRoleAttribute))"
    for (label, name) in [("sub", kAXSubroleAttribute), ("title", kAXTitleAttribute), ("id", "AXIdentifier"),
                          ("desc", kAXDescriptionAttribute), ("value", kAXValueAttribute), ("enabled", kAXEnabledAttribute),
                          ("focused", kAXFocusedAttribute), ("cmd", "AXMenuItemCmdChar"), ("mods", "AXMenuItemCmdModifiers")] {
        let v = str(e, name)
        if !v.isEmpty { line += " | \(label)=\(v.prefix(80))" }
    }
    if attr(e, "AXDefaultButton") != nil { line += " | HAS_DEFAULT_BUTTON" }
    if attr(e, "AXCancelButton") != nil { line += " | HAS_CANCEL_BUTTON" }
    let a = actions(e).filter { $0 != "AXShowMenu" && $0 != "AXScrollToVisible" }
    if !a.isEmpty { line += " | actions=\(a.joined(separator: ","))" }
    print(line)
    if depth >= maxDepth { return }
    let kids = children(e)
    // パラメータの一覧のような大きな部分は先頭だけ
    for (i, k) in kids.enumerated() {
        if i >= 40 { print("\(pad)  ... (\(kids.count - 40) more)"); break }
        dump(k, depth + 1, maxDepth)
    }
}
func key(_ code: CGKeyCode, _ flags: CGEventFlags = []) {
    for down in [true, false] {
        let ev = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)!
        ev.flags = flags
        ev.post(tap: .cghidEventTap)
        usleep(30_000)
    }
}
func pause(_ s: Double) { usleep(useconds_t(s * 1_000_000)) }

guard AXIsProcessTrusted() else { print("アクセシビリティの許可がありません (このターミナルに許可を)"); exit(1) }
guard let hx = NSRunningApplication.runningApplications(withBundleIdentifier: "com.line6.hxedit").first else {
    print("HX Edit が起動していません"); exit(1)
}
let app = AXUIElementCreateApplication(hx.processIdentifier)
hx.activate()
pause(1)

func fileMenuItem(_ title: String) -> AXUIElement? {
    guard let bar = attr(app, kAXMenuBarAttribute) else { return nil }
    let barEl = bar as! AXUIElement
    for top in children(barEl) where str(top, kAXTitleAttribute) == "File" {
        for menu in children(top) {
            for item in children(menu) where str(item, kAXTitleAttribute) == title { return item }
        }
    }
    return nil
}
func windows(_ label: String) {
    print("\n===== \(label) =====")
    print("focused window:")
    if let f = attr(app, kAXFocusedWindowAttribute) { dump(f as! AXUIElement, 1, 1) }
    for w in (attr(app, kAXWindowsAttribute) as? [AXUIElement]) ?? [] {
        dump(w, 0, 12)
    }
}

print("===== File メニュー =====")
for t in ["Save Preset", "Save Preset As...", "Import Preset...", "Export Preset..."] {
    if let i = fileMenuItem(t) { dump(i, 0, 0) } else { print("見つからない: \(t)") }
}

windows("はじめ")

for (title, label) in [("Export Preset...", "書き出し"), ("Import Preset...", "読み込み")] {
    guard let item = fileMenuItem(title) else { print("見つからない: \(title)"); continue }
    let r = AXUIElementPerformAction(item, kAXPressAction as CFString)
    print("\n\(title) を押した: \(r.rawValue)")
    pause(2)
    windows("\(label)の画面")
    key(5, [.maskCommand, .maskShift]) // Cmd+Shift+G
    pause(1.5)
    windows("\(label)の画面で Cmd+Shift+G")
    key(53) // Escape
    pause(1)
    key(53)
    pause(1.5)
    windows("\(label)を Escape で閉じた後")
}
print("\nおわり")
