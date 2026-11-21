// Valeton Suite (GP-5 のエディタ) 風の模擬スクリーンショットを描く。
// AI 結合テスト (ai_live_test.rs) と、スライダーの画素読み取り (slider.rs) のテストの入力に使う。
// 配置と色は実物のスクショに合わせてある。
//
// 使い方: swift scripts/make_mock_valeton.swift src-tauri/tests/fixtures/mock_valeton.png
//
// ウィンドウは 1000×773 ポイント (タイトルバー 28 を含む) を 2 倍の 2000×1546 ピクセルで描く。
// スライダーは x 487〜938、i 本目の縦は 165 + 42 * i から高さ 30 (ポイント)。

import AppKit

let args = CommandLine.arguments
guard args.count == 2 else {
    FileHandle.standardError.write("usage: make_mock_valeton.swift <out.png>\n".data(using: .utf8)!)
    exit(2)
}

let W: CGFloat = 1000, H: CGFloat = 773, scale: CGFloat = 2
let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: Int(W * scale), pixelsHigh: Int(H * scale),
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: W, height: H)

NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
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

func stroke(_ r: NSRect, _ c: NSColor, radius: CGFloat, width: CGFloat) {
    c.setStroke()
    let p = NSBezierPath(roundedRect: r, xRadius: radius, yRadius: radius)
    p.lineWidth = width
    p.stroke()
}

func text(_ s: String, at p: NSPoint, size: CGFloat, color: NSColor = .white, bold: Bool = false, center: Bool = true) {
    let font = bold ? NSFont.boldSystemFont(ofSize: size) : NSFont.systemFont(ofSize: size)
    let str = NSAttributedString(string: s, attributes: [.font: font, .foregroundColor: color])
    let sz = str.size()
    ctx.saveGState()
    ctx.translateBy(x: center ? p.x - sz.width / 2 : p.x, y: p.y + sz.height / 2)
    ctx.scaleBy(x: 1, y: -1)
    str.draw(at: .zero)
    ctx.restoreGState()
}

let bg = rgb(22, 26, 34)
let panel = rgb(38, 43, 54)
let fillBlue = rgb(59, 116, 230)
let gray = rgb(70, 74, 84)

// 背景とタイトルバー
fill(NSRect(x: 0, y: 0, width: W, height: H), bg)
fill(NSRect(x: 0, y: 0, width: W, height: 28), rgb(44, 46, 52))
for (i, c) in [NSColor.systemRed, .systemYellow, .systemGreen].enumerated() {
    fill(NSRect(x: 12 + CGFloat(i) * 20, y: 8, width: 12, height: 12), c, radius: 6)
}
text("Valeton Suite", at: NSPoint(x: W / 2, y: 14), size: 13)

let top: CGFloat = 28
// パッチ名
fill(NSRect(x: 38, y: top + 19, width: 745, height: 31), panel, radius: 8)
text("01", at: NSPoint(x: 48, y: top + 35), size: 12, center: false)
text("Neo Soul", at: NSPoint(x: 418, y: top + 35), size: 12)

// タブ
fill(NSRect(x: 17, y: top + 73, width: 447, height: 27), panel, radius: 8)
fill(NSRect(x: 17, y: top + 73, width: 223, height: 27), fillBlue, radius: 8)
text("Edit", at: NSPoint(x: 128, y: top + 86), size: 12)
text("Patch Settings", at: NSPoint(x: 351, y: top + 86), size: 12)

// モジュール (On は色付き、Off は灰色)。AMP を選んでいる
let modules: [(String, NSColor, Bool)] = [
    ("NR", rgb(210, 212, 216), true), ("PRE", rgb(160, 70, 210), true), ("DST", rgb(220, 70, 80), true),
    ("N→S", gray, false), ("AMP", rgb(225, 160, 60), true),
    ("CAB", rgb(190, 120, 50), true), ("EQ", rgb(220, 200, 150), true), ("MOD", gray, false),
    ("DLY", gray, false), ("RVB", rgb(50, 110, 220), true),
]
for (i, m) in modules.enumerated() {
    let col = CGFloat(i % 5), row = CGFloat(i / 5)
    let r = NSRect(x: 18 + col * 90, y: top + 123 + row * 72, width: 84, height: 56)
    if m.0 == "AMP" {
        fill(r, rgb(52, 58, 72), radius: 6)
        stroke(r, fillBlue, radius: 6, width: 2)
    }
    fill(NSRect(x: r.midX - 17, y: r.minY + 5, width: 34, height: 46), m.1, radius: 5)
    text(m.0, at: NSPoint(x: r.midX, y: r.minY + 40), size: 12, color: m.2 ? .white : rgb(140, 140, 145), bold: true)
}

// 右: 選んだモジュールのモデルとパラメータ
fill(NSRect(x: 487, y: top + 73, width: 100, height: 51), panel, radius: 8)
text("AMP", at: NSPoint(x: 537, y: top + 88), size: 12)
fill(NSRect(x: 516, y: top + 99, width: 43, height: 17), fillBlue, radius: 8.5)
fill(NSRect(x: 595, y: top + 73, width: 343, height: 51), panel, radius: 8)
text("UK 800", at: NSPoint(x: 749, y: top + 88), size: 12)
text("Drive", at: NSPoint(x: 749, y: top + 108), size: 12)
text(">", at: NSPoint(x: 916, y: top + 98), size: 14)

let params: [(String, Int)] = [("Gain", 60), ("PRES", 50), ("VOL", 70), ("Bass", 50), ("Middle", 45), ("Treble", 65)]
for (i, p) in params.enumerated() {
    let y = 165 + 42 * CGFloat(i)
    let bar = NSRect(x: 487, y: y, width: 451, height: 30)
    fill(bar, panel, radius: 6)
    if p.1 > 0 {
        // 左端は丸く、塗りの右端はまっすぐ
        ctx.saveGState()
        NSBezierPath(roundedRect: bar, xRadius: 6, yRadius: 6).addClip()
        fill(NSRect(x: bar.minX, y: bar.minY, width: bar.width * CGFloat(p.1) / 100, height: bar.height), fillBlue)
        ctx.restoreGState()
    }
    text(p.0, at: NSPoint(x: bar.minX + 8, y: bar.midY), size: 12, center: false)
    text("\(p.1)", at: NSPoint(x: bar.maxX - 14, y: bar.midY), size: 12)
}

// 右端のメニュー
fill(NSRect(x: 955, y: top + 18, width: 45, height: H - top - 18), rgb(30, 34, 42), radius: 10)
for (i, s) in ["Edit", "Drum", "", "Global", "Manage"].enumerated() where !s.isEmpty {
    text(s, at: NSPoint(x: 978, y: top + 180 + CGFloat(i) * 136), size: 10, color: i == 0 ? .white : rgb(120, 120, 125))
}

NSGraphicsContext.restoreGraphicsState()
let png = rep.representation(using: .png, properties: [:])!
try! png.write(to: URL(fileURLWithPath: args[1]))
print("wrote \(args[1]) (\(rep.pixelsWide)x\(rep.pixelsHigh))")
