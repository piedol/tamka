// Renders the Tamka logo: speech turning into text. A graphite squircle (Photon's
// family material), white waveform bars, and the amber caret they become.
// Usage: swift scripts/make-icon.swift   → assets/AppIcon.icns, assets/logo.png
import AppKit

let amber = NSColor(srgbRed: 1.0, green: 0.70, blue: 0.25, alpha: 1)

func draw(_ px: CGFloat) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(px), pixelsHigh: Int(px), bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let ctx = NSGraphicsContext.current!.cgContext
    ctx.scaleBy(x: px / 1024, y: px / 1024)
    let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

    // Squircle body on the macOS icon grid (824pt body centred in 1024).
    let body = CGRect(x: 100, y: 100, width: 824, height: 824)
    let path = CGPath(roundedRect: body, cornerWidth: 186, cornerHeight: 186, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 28, color: NSColor(white: 0, alpha: 0.35).cgColor)
    ctx.addPath(path); ctx.setFillColor(NSColor(srgbRed: 0.08, green: 0.08, blue: 0.10, alpha: 1).cgColor); ctx.fillPath()
    ctx.restoreGState()
    ctx.saveGState()
    ctx.addPath(path); ctx.clip()
    let bg = CGGradient(colorsSpace: srgb, colors: [
        NSColor(srgbRed: 0.17, green: 0.17, blue: 0.21, alpha: 1).cgColor,
        NSColor(srgbRed: 0.06, green: 0.06, blue: 0.08, alpha: 1).cgColor] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(bg, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])

    // The waveform: an utterance's envelope, quiet–loud–quiet.
    let mid: CGFloat = 512
    let barW: CGFloat = 46
    let bars: [(x: CGFloat, h: CGFloat)] = [(292, 190), (376, 370), (460, 500), (544, 330), (628, 170)]
    ctx.setFillColor(NSColor(white: 1, alpha: 0.95).cgColor)
    for b in bars {
        let r = CGRect(x: b.x - barW / 2, y: mid - b.h / 2, width: barW, height: b.h)
        ctx.addPath(CGPath(roundedRect: r, cornerWidth: barW / 2, cornerHeight: barW / 2, transform: nil))
        ctx.fillPath()
    }

    // The caret it becomes: amber, taller and thinner, with a soft glow.
    let caretX: CGFloat = 738
    let glow = CGGradient(colorsSpace: srgb, colors: [
        amber.withAlphaComponent(0.42).cgColor, amber.withAlphaComponent(0).cgColor] as CFArray, locations: [0, 1])!
    ctx.drawRadialGradient(glow, startCenter: CGPoint(x: caretX, y: mid), startRadius: 0,
                           endCenter: CGPoint(x: caretX, y: mid), endRadius: 230, options: [])
    let caretW: CGFloat = 34
    let caret = CGRect(x: caretX - caretW / 2, y: mid - 270, width: caretW, height: 540)
    ctx.addPath(CGPath(roundedRect: caret, cornerWidth: caretW / 2, cornerHeight: caretW / 2, transform: nil))
    ctx.setFillColor(amber.cgColor)
    ctx.fillPath()
    ctx.restoreGState()
    NSGraphicsContext.restoreGraphicsState()
    return rep
}

/// The menu-bar glyph: a microphone and the caret its words appear at, as a monochrome
/// template image (22 x 18 pt), so macOS tints it for light and dark menu bars and Tamka
/// can tint it red while recording.
func menuGlyph(scale: CGFloat) -> NSBitmapImageRep {
    let (w, h): (CGFloat, CGFloat) = (22, 18)
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(w * scale), pixelsHigh: Int(h * scale),
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = NSSize(width: w, height: h)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    // The rep's size already maps points to its pixels, so drawing is in points at any scale.
    let ctx = NSGraphicsContext.current!.cgContext
    ctx.setFillColor(NSColor.black.cgColor)
    ctx.setStrokeColor(NSColor.black.cgColor)
    // The microphone: a filled capsule, the holder around its base, the stem and the foot.
    let (cx, capW, capH, capBottom, line): (CGFloat, CGFloat, CGFloat, CGFloat, CGFloat) = (8, 5.6, 9.2, 7.2, 1.5)
    let cap = CGRect(x: cx - capW / 2, y: capBottom, width: capW, height: capH)
    ctx.addPath(CGPath(roundedRect: cap, cornerWidth: capW / 2, cornerHeight: capW / 2, transform: nil))
    ctx.fillPath()
    ctx.setLineWidth(line)
    ctx.setLineCap(.round)
    let r = capW / 2 + 2.3, arcY = capBottom + 2.6
    ctx.move(to: CGPoint(x: cx - r, y: arcY + 1.6))
    ctx.addLine(to: CGPoint(x: cx - r, y: arcY))
    ctx.addArc(center: CGPoint(x: cx, y: arcY), radius: r, startAngle: .pi, endAngle: 0, clockwise: false)
    ctx.addLine(to: CGPoint(x: cx + r, y: arcY + 1.6))
    ctx.move(to: CGPoint(x: cx, y: arcY - r))
    ctx.addLine(to: CGPoint(x: cx, y: 1.2))
    ctx.move(to: CGPoint(x: cx - 2.6, y: 1.2))
    ctx.addLine(to: CGPoint(x: cx + 2.6, y: 1.2))
    ctx.strokePath()
    // The caret.
    let caret = CGRect(x: 17.5 - 0.9, y: 4.5, width: 1.8, height: 12.1)
    ctx.addPath(CGPath(roundedRect: caret, cornerWidth: 0.9, cornerHeight: 0.9, transform: nil))
    ctx.fillPath()
    NSGraphicsContext.restoreGraphicsState()
    return rep
}

let fm = FileManager.default
try? fm.createDirectory(at: URL(fileURLWithPath: "assets"), withIntermediateDirectories: true)
let set = URL(fileURLWithPath: "assets/AppIcon.iconset")
try? fm.removeItem(at: set)
try! fm.createDirectory(at: set, withIntermediateDirectories: true)
for (pt, scale) in [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)] {
    let name = scale == 1 ? "icon_\(pt)x\(pt).png" : "icon_\(pt)x\(pt)@2x.png"
    let data = draw(CGFloat(pt * scale)).representation(using: .png, properties: [:])!
    try! data.write(to: set.appendingPathComponent(name))
}
try! draw(1024).representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "assets/logo.png"))
let p = Process()
p.launchPath = "/usr/bin/iconutil"
p.arguments = ["-c", "icns", set.path, "-o", "assets/AppIcon.icns"]
p.launch()
p.waitUntilExit()
try? fm.removeItem(at: set)
try! menuGlyph(scale: 1).representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "assets/MenuIcon.png"))
try! menuGlyph(scale: 2).representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "assets/MenuIcon@2x.png"))
print("Wrote assets/AppIcon.icns, assets/logo.png and assets/MenuIcon(@2x).png")
