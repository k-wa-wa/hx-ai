// HX Edit 風の模擬スクリーンショットを描く。AI 結合テスト (ai_live_test.rs) の入力に使う。
// 実物の HX Edit を起動せずに、座標の読み取りと JSON の往復を確かめるためのもの。
// 配置と色は実物 (HX Edit 3.x、HX Stomp XL) のスクショに合わせてある。
//
// 使い方: swift scripts/make_mock_screenshot.swift src-tauri/tests/fixtures/mock_hx_edit.png
//
// ウィンドウは 1000×604 ポイントを 2 倍 (Retina 相当) の 2000×1208 ピクセルで描く。
// - シグナルチェーン: Dist (370, 70)・Amp (618, 70, 選択中)・Delay (800, 70)
// - モデル名の行の右端に星 (951, 174) と電源 (975, 174)。押してはいけない
// - パラメータ i 行目: 名前と数値の高さ 194 + 25.7 * i、数値の中心 x 956、上下の矢印 x 974、バー 422〜974

import AppKit

let args = CommandLine.arguments
guard args.count == 2 else {
    FileHandle.standardError.write("usage: make_mock_screenshot.swift <out.png>\n".data(using: .utf8)!)
    exit(2)
}

let W: CGFloat = 1000, H: CGFloat = 604, scale: CGFloat = 2
let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: Int(W * scale), pixelsHigh: Int(H * scale),
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: W, height: H)

NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
// 左上原点で描けるように反転する
let ctx = NSGraphicsContext.current!.cgContext
ctx.translateBy(x: 0, y: H)
ctx.scaleBy(x: 1, y: -1)

func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat) -> NSColor {
    NSColor(deviceRed: r / 255, green: g / 255, blue: b / 255, alpha: 1)
}

func fill(_ r: NSRect, _ c: NSColor, radius: CGFloat = 0) {
    c.setFill()
    NSBezierPath(roundedRect: r, xRadius: radius, yRadius: radius).fill()
}

func stroke(_ r: NSRect, _ c: NSColor, radius: CGFloat = 0, width: CGFloat = 1) {
    c.setStroke()
    let p = NSBezierPath(roundedRect: r, xRadius: radius, yRadius: radius)
    p.lineWidth = width
    p.stroke()
}

/// `center` が true なら中心、false なら左端を `p` に合わせて描く (縦はどちらも中心)。
func text(_ s: String, at p: NSPoint, size: CGFloat, color: NSColor = .white, bold: Bool = false, center: Bool = false) {
    let font = bold ? NSFont.boldSystemFont(ofSize: size) : NSFont.systemFont(ofSize: size)
    let str = NSAttributedString(string: s, attributes: [.font: font, .foregroundColor: color])
    let sz = str.size()
    ctx.saveGState()
    ctx.translateBy(x: center ? p.x - sz.width / 2 : p.x, y: p.y + sz.height / 2)
    ctx.scaleBy(x: 1, y: -1)
    str.draw(at: .zero)
    ctx.restoreGState()
}

func triangle(center c: NSPoint, width: CGFloat, height: CGFloat, up: Bool, color: NSColor) {
    let p = NSBezierPath()
    let tip = up ? c.y - height / 2 : c.y + height / 2
    let base = up ? c.y + height / 2 : c.y - height / 2
    p.move(to: NSPoint(x: c.x, y: tip))
    p.line(to: NSPoint(x: c.x - width / 2, y: base))
    p.line(to: NSPoint(x: c.x + width / 2, y: base))
    p.close()
    color.setFill()
    p.fill()
}

let bg = rgb(26, 26, 28)
let panel = rgb(38, 38, 42)
let orange = rgb(236, 150, 50)
let barDark = rgb(110, 66, 20)

// 背景とタイトルバー
fill(NSRect(x: 0, y: 0, width: W, height: H), bg)
fill(NSRect(x: 0, y: 0, width: W, height: 18), rgb(50, 50, 54))
for (i, c) in [NSColor.systemRed, .systemYellow, .systemGreen].enumerated() {
    fill(NSRect(x: 6 + CGFloat(i) * 14, y: 4, width: 10, height: 10), c, radius: 5)
}
text("HX Edit - HX Stomp XL", at: NSPoint(x: W / 2, y: 9), size: 9, color: .lightGray, bold: true, center: true)

// 左: プリセットの一覧
fill(NSRect(x: 4, y: 22, width: 152, height: 556), rgb(20, 20, 22))
text("Presets", at: NSPoint(x: 22, y: 32), size: 9, bold: true)
text("IRs", at: NSPoint(x: 78, y: 32), size: 9)
text("Favorites", at: NSPoint(x: 120, y: 32), size: 9)
text("SETLIST", at: NSPoint(x: 11, y: 54), size: 8, color: orange)
text("PRESETS", at: NSPoint(x: 11, y: 75), size: 8, bold: true)
for i in 0..<33 {
    let y = 100 + CGFloat(i) * 14.6
    let slot = 24 + (i + 3) / 4
    let name = "\(slot)\(["A", "B", "C", "D"][(i + 3) % 4])  New Preset"
    if i == 17 { fill(NSRect(x: 6, y: y - 7, width: 143, height: 14), rgb(70, 70, 60)) }
    text(name, at: NSPoint(x: 11, y: y), size: 8.5, color: i == 17 ? orange : .white)
}

// 上: プリセット名とスナップショット
fill(NSRect(x: 180, y: 24, width: 160, height: 20), panel, radius: 3)
text("28B  New Preset", at: NSPoint(x: 185, y: 34), size: 10)
fill(NSRect(x: 345, y: 24, width: 120, height: 20), panel, radius: 3)
text("SNAPSHOT 1", at: NSPoint(x: 365, y: 34), size: 8.5)
fill(NSRect(x: 958, y: 24, width: 34, height: 20), panel, radius: 3)
text("70.4", at: NSPoint(x: 975, y: 34), size: 10, center: true)

// シグナルチェーン (枠の中に、ブロックのアイコンと名前)
stroke(NSRect(x: 165, y: 50, width: 827, height: 88), rgb(40, 40, 140), radius: 4, width: 2)
fill(NSRect(x: 205, y: 69, width: 745, height: 2), rgb(90, 90, 95))
let chain: [(String, CGFloat, NSColor, Bool)] = [
    ("Main L/R", 205, .lightGray, false), ("Dist", 370, orange, false),
    ("Amp", 618, rgb(220, 60, 50), true), ("Delay", 800, rgb(70, 180, 90), false),
    ("Main L/R", 950, .lightGray, false),
]
for b in chain {
    let r = NSRect(x: b.1 - 10, y: 60, width: 20, height: 20)
    if b.3 { stroke(r.insetBy(dx: -2, dy: -2), .white, radius: 4, width: 1.5) }
    fill(r, rgb(30, 30, 32), radius: 4)
    stroke(r, b.2, radius: 4, width: 1.5)
    text(b.0, at: NSPoint(x: b.1, y: 86), size: 7.5, color: b.2, center: true)
}

// 下: EDIT タブ
fill(NSRect(x: 165, y: 144, width: 412, height: 16), rgb(48, 48, 52))
text("EDIT", at: NSPoint(x: 371, y: 152), size: 8, bold: true, center: true)
fill(NSRect(x: 578, y: 144, width: 414, height: 16), rgb(36, 36, 40))
text("BYPASS/CONTROLLER ASSIGN", at: NSPoint(x: 787, y: 152), size: 8, color: .lightGray, bold: true, center: true)

// 左: カテゴリとモデルの一覧 (押すとモデルが変わる)
fill(NSRect(x: 166, y: 165, width: 237, height: 412), rgb(22, 22, 24))
text("Amp", at: NSPoint(x: 188, y: 174), size: 9, color: rgb(220, 60, 50))
text("Mono", at: NSPoint(x: 289, y: 174), size: 9)
let categories: [(String, NSColor)] = [
    ("None", .white), ("Favorites", .white), ("Distortion", orange), ("Dynamics", rgb(220, 200, 60)),
    ("EQ", rgb(220, 200, 60)), ("Modulation", rgb(60, 140, 230)), ("Delay", rgb(70, 180, 90)),
    ("Reverb", rgb(230, 90, 60)), ("Pitch/Synth", rgb(170, 80, 220)), ("Filter", rgb(150, 80, 200)),
    ("Wah", rgb(150, 80, 200)), ("Amp+Cab", rgb(220, 60, 50)), ("Amp", rgb(220, 60, 50)),
    ("Preamp", rgb(220, 60, 50)), ("Cab", rgb(220, 60, 50)), ("IR", rgb(220, 60, 90)),
    ("Volume/Pan", rgb(80, 180, 170)), ("Send/Return", rgb(80, 180, 170)), ("Looper", .gray),
]
for (i, c) in categories.enumerated() {
    let y = 191 + CGFloat(i) * 16.7
    if c.0 == "Amp" { fill(NSRect(x: 167, y: y - 8, width: 117, height: 16), rgb(45, 45, 48)) }
    text(c.0, at: NSPoint(x: 189, y: y), size: 8.5, color: c.1)
}
let models = [
    "WhoWatt 100", "Soup Pro", "Stone Age 185", "Voltage Queen", "Tweed Blues Nrm", "Tweed Blues Brt",
    "Fullerton Nrm", "Fullerton Brt", "US Small Tweed", "US Princess", "US Deluxe Nrm", "US Deluxe Vib",
    "US Double Nrm", "US Double Vib", "Mail Order Twin", "Divided Duo", "Interstate Zed", "Jazz Rivet 120",
    "Essex A15", "Essex A30", "Matchstick Ch1", "Matchstick Ch2", "Mandarin 80",
]
for (i, m) in models.enumerated() {
    let y = 191 + CGFloat(i) * 16.7
    if m == "US Deluxe Nrm" { fill(NSRect(x: 286, y: y - 8, width: 112, height: 16), rgb(45, 45, 48)) }
    fill(NSRect(x: 290, y: y - 5, width: 10, height: 10), rgb(90, 90, 95), radius: 2)
    text(m, at: NSPoint(x: 308, y: y), size: 8.5)
}

// 右: 選択中のブロックのモデル名と、パラメータ
fill(NSRect(x: 405, y: 165, width: 587, height: 412), rgb(16, 16, 18))
fill(NSRect(x: 405, y: 165, width: 587, height: 18), rgb(36, 36, 40))
fill(NSRect(x: 412, y: 169, width: 12, height: 10), rgb(220, 60, 50), radius: 2)
text("US Deluxe Nrm", at: NSPoint(x: 430, y: 174), size: 9)
text("☆", at: NSPoint(x: 951, y: 174), size: 13, color: .white, center: true)
text("⏻", at: NSPoint(x: 975, y: 174), size: 12, color: .white, center: true)

let params: [(String, Double)] = [
    ("Drive", 5.0), ("Bass", 5.0), ("Mid", 6.0), ("Treble", 7.2),
    ("Presence", 4.5), ("Ch Vol", 7.0), ("Master", 6.0),
]
for (i, p) in params.enumerated() {
    let y = 194 + CGFloat(i) * 25.7
    text(p.0, at: NSPoint(x: 422, y: y), size: 8.5, color: orange)
    text(String(format: "%.1f", p.1), at: NSPoint(x: 956, y: y), size: 8.5, color: orange, center: true)
    // 上下の矢印: 実物と同じく、ブロックの色の小さな三角を 2 つ縦に並べる
    triangle(center: NSPoint(x: 974, y: y - 4.5), width: 5.5, height: 2.8, up: true, color: orange)
    triangle(center: NSPoint(x: 974, y: y + 4.5), width: 5.5, height: 2.8, up: false, color: orange)
    let barY = y + 7
    let end = 422 + 552 * CGFloat(p.1) / 10
    fill(NSRect(x: 422, y: barY, width: end - 422, height: 5), barDark)
    fill(NSRect(x: end - 4, y: barY - 2, width: 8, height: 9), orange, radius: 1.5)
}

// 下端の状態表示
text("HX Stomp XL", at: NSPoint(x: 521, y: 592), size: 8, bold: true, center: true)
text("No updates available.", at: NSPoint(x: 940, y: 592), size: 8, color: .gray, center: true)

NSGraphicsContext.restoreGraphicsState()
let png = rep.representation(using: .png, properties: [:])!
try! png.write(to: URL(fileURLWithPath: args[1]))
print("wrote \(args[1]) (\(rep.pixelsWide)x\(rep.pixelsHigh))")
