// Composes docs/images/showcase.png for the README from real screenshots in
// docs/images: the Models window, the menu and the listening pill on a graphite
// backdrop with the logo's amber glow.
// Usage: swift scripts/make-showcase.swift
import AppKit

let dir = "docs/images/"
func image(_ name: String) -> NSImage {
    guard let img = NSImage(contentsOfFile: dir + name) else { fatalError("missing \(dir + name)") }
    return img
}
func pixels(_ img: NSImage) -> NSSize {
    let rep = img.representations.first!
    return NSSize(width: rep.pixelsWide, height: rep.pixelsHigh)
}

let (W, H): (CGFloat, CGFloat) = (2400, 1480)
// Every screenshot is a 2x capture; drawing them at one scale keeps their text the same size.
let S: CGFloat = 0.88
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(W), pixelsHigh: Int(H), bitsPerSample: 8,
                           samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                           bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let ctx = NSGraphicsContext.current!.cgContext
let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

// Layout: Settings › Models on the left; name, menu and listening pill on the right.
let leftX: CGFloat = 120, top: CGFloat = 96
let rightCX = leftX + 1200 * S + (W - leftX - 1200 * S) / 2
let pillTop: CGFloat = 1090

// Backdrop: rounded graphite card, amber glow behind the pill.
let card = CGPath(roundedRect: CGRect(x: 0, y: 0, width: W, height: H), cornerWidth: 56, cornerHeight: 56, transform: nil)
ctx.addPath(card)
ctx.clip()
let bg = CGGradient(colorsSpace: srgb, colors: [
    NSColor(srgbRed: 0.15, green: 0.15, blue: 0.19, alpha: 1).cgColor,
    NSColor(srgbRed: 0.05, green: 0.05, blue: 0.07, alpha: 1).cgColor] as CFArray, locations: [0, 1])!
ctx.drawLinearGradient(bg, start: CGPoint(x: 0, y: H), end: CGPoint(x: W, y: 0), options: [])
let amber = NSColor(srgbRed: 1.0, green: 0.70, blue: 0.25, alpha: 1)
let glow = CGGradient(colorsSpace: srgb, colors: [amber.withAlphaComponent(0.22).cgColor, amber.withAlphaComponent(0).cgColor] as CFArray,
                      locations: [0, 1])!
let glowAt = CGPoint(x: rightCX, y: H - pillTop - 34)
ctx.drawRadialGradient(glow, startCenter: glowAt, startRadius: 0, endCenter: glowAt, endRadius: 720, options: [])

/// Draw an image with a soft drop shadow, `width` pixels wide, top-left at (x, top).
func place(_ img: NSImage, x: CGFloat, top: CGFloat, width: CGFloat, shadow: Bool = true) {
    let px = pixels(img)
    let h = width * px.height / px.width
    let r = CGRect(x: x, y: H - top - h, width: width, height: h)
    ctx.saveGState()
    if shadow {
        ctx.setShadow(offset: CGSize(width: 0, height: -18), blur: 60, color: NSColor(white: 0, alpha: 0.55).cgColor)
    }
    img.draw(in: r, from: .zero, operation: .sourceOver, fraction: 1)
    ctx.restoreGState()
}
/// Screenshots at the shared scale, centred on `cx` when given.
func shot(_ name: String, x: CGFloat? = nil, cx: CGFloat? = nil, top: CGFloat) {
    let img = image(name)
    let w = pixels(img).width * S
    place(img, x: x ?? (cx! - w / 2), top: top, width: w)
}

shot("settings-models.png", x: leftX, top: top)

// Name block: logo, wordmark and one line of copy, centred over the menu.
let logo = NSImage(contentsOfFile: "assets/logo.png")!
let logoW: CGFloat = 168, gap: CGFloat = 34
let name = NSAttributedString(string: "Tamka", attributes: [
    .font: NSFont.systemFont(ofSize: 104, weight: .bold), .foregroundColor: NSColor.white])
let line = NSAttributedString(string: "Private dictation for your Mac", attributes: [
    .font: NSFont.systemFont(ofSize: 40, weight: .medium), .foregroundColor: NSColor(white: 1, alpha: 0.62)])
let textW = max(name.size().width, line.size().width)
let blockX = rightCX - (logoW + gap + textW) / 2
let blockTop: CGFloat = 170
place(logo, x: blockX, top: blockTop, width: logoW, shadow: false)
let textX = blockX + logoW + gap
name.draw(at: NSPoint(x: textX, y: H - blockTop - 112))
line.draw(at: NSPoint(x: textX, y: H - blockTop - 166))

shot("menu.png", cx: rightCX, top: 450)
shot("listening.png", cx: rightCX, top: pillTop)

NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: dir + "showcase.png"))
print("Wrote \(dir)showcase.png")
