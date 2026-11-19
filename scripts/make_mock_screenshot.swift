// HX Edit 風の模擬スクリーンショットを描く。AI 結合テスト (ai_live_test.rs) の入力に使う。
// 実物の HX Edit を起動せずに、座標の読み取りと JSON の往復を確かめるためのもの。
//
// 使い方: swift scripts/make_mock_screenshot.swift src-tauri/tests/fixtures/mock_hx_edit.png
//
// ウィンドウは 1200×800 ポイントを 2 倍 (Retina 相当) の 2400×1600 ピクセルで描く。
// パラメータの数値欄の中心 (ポイント) は下の `params` の x と valueY で決まる。

import AppKit

let args = CommandLine.arguments
guard args.count == 2 else {
    FileHandle.standardError.write("usage: make_mock_screenshot.swift <out.png>\n".data(using: .utf8)!)
    exit(2)
}

let W: CGFloat = 1200, H: CGFloat = 800, scale: CGFloat = 2
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

func fill(_ r: NSRect, _ c: NSColor, radius: CGFloat = 0) {
    c.setFill()
    NSBezierPath(roundedRect: r, xRadius: radius, yRadius: radius).fill()
}

func text(_ s: String, center: NSPoint, size: CGFloat, color: NSColor = .white, bold: Bool = false) {
    let font = bold ? NSFont.boldSystemFont(ofSize: size) : NSFont.systemFont(ofSize: size)
    let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color]
    let str = NSAttributedString(string: s, attributes: attrs)
    let sz = str.size()
    // 反転した座標系で文字が逆さにならないよう、その場だけ戻して描く
    ctx.saveGState()
    ctx.translateBy(x: center.x - sz.width / 2, y: center.y + sz.height / 2)
    ctx.scaleBy(x: 1, y: -1)
    str.draw(at: .zero)
    ctx.restoreGState()
}

// 背景とタイトルバー
fill(NSRect(x: 0, y: 0, width: W, height: H), NSColor(white: 0.13, alpha: 1))
fill(NSRect(x: 0, y: 0, width: W, height: 28), NSColor(white: 0.25, alpha: 1))
for (i, c) in [NSColor.systemRed, .systemYellow, .systemGreen].enumerated() {
    fill(NSRect(x: 12 + CGFloat(i) * 20, y: 8, width: 12, height: 12), c, radius: 6)
}
text("HX Edit", center: NSPoint(x: W / 2, y: 14), size: 13)

// ツールバー
fill(NSRect(x: 0, y: 28, width: W, height: 60), NSColor(white: 0.18, alpha: 1))
text("01A  Clean Lead", center: NSPoint(x: 160, y: 58), size: 18, bold: true)
text("Setlist: FACTORY 1", center: NSPoint(x: 950, y: 58), size: 13, color: .lightGray)

// シグナルチェーン
fill(NSRect(x: 40, y: 150, width: W - 80, height: 4), NSColor(white: 0.4, alpha: 1))
let blocks: [(String, String, NSColor, Bool)] = [
    ("Input", "Multi", .darkGray, false),
    ("Distortion", "Scream 808", .systemOrange, false),
    ("Amp", "US Deluxe Nrm", .systemRed, true),
    ("Delay", "Transistor Tape", .systemGreen, false),
    ("Reverb", "Plate", .systemBlue, false),
    ("Output", "Main", .darkGray, false),
]
for (i, b) in blocks.enumerated() {
    let cx = 120 + CGFloat(i) * 192
    let r = NSRect(x: cx - 50, y: 112, width: 100, height: 80)
    if b.3 { fill(r.insetBy(dx: -5, dy: -5), .white, radius: 12) }
    fill(r, b.2, radius: 10)
    text(b.0, center: NSPoint(x: cx, y: 145), size: 13, bold: true)
    text(b.1, center: NSPoint(x: cx, y: 215), size: 12, color: .lightGray)
}

// パラメータ欄 (選択中の Amp ブロック)
fill(NSRect(x: 0, y: 520, width: W, height: H - 520), NSColor(white: 0.17, alpha: 1))
text("US Deluxe Nrm", center: NSPoint(x: 140, y: 548), size: 16, bold: true)
let params: [(String, String)] = [
    ("Drive", "5.0"), ("Bass", "6.0"), ("Mid", "5.5"), ("Treble", "7.2"),
    ("Presence", "4.0"), ("Ch Vol", "6.0"), ("Master", "8.5"), ("Sag", "5.0"),
]
let labelY: CGFloat = 600, knobY: CGFloat = 650, valueY: CGFloat = 712
for (i, p) in params.enumerated() {
    let x = 100 + CGFloat(i) * 140
    text(p.0, center: NSPoint(x: x, y: labelY), size: 14, color: .lightGray)
    fill(NSRect(x: x - 28, y: knobY - 28, width: 56, height: 56), NSColor(white: 0.35, alpha: 1), radius: 28)
    fill(NSRect(x: x - 3, y: knobY - 26, width: 6, height: 18), .white, radius: 3)
    fill(NSRect(x: x - 34, y: valueY - 13, width: 68, height: 26), NSColor(white: 0.08, alpha: 1), radius: 4)
    text(p.1, center: NSPoint(x: x, y: valueY), size: 15)
}

NSGraphicsContext.restoreGraphicsState()
let png = rep.representation(using: .png, properties: [:])!
try! png.write(to: URL(fileURLWithPath: args[1]))
print("wrote \(args[1]) (\(rep.pixelsWide)x\(rep.pixelsHigh))")
