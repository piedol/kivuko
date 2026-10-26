// Renders the Kivuko icon: a crossing. A graphite squircle (the family material of Photon,
// Tamka and Kipimo), two white shores, the violet arc of the crossing between them, and the
// water it carries you over.
// Usage: swift scripts/make-icon.swift   → assets/AppIcon.icns, assets/logo.png
import AppKit

let violet = NSColor(srgbRed: 0.72, green: 0.55, blue: 1.0, alpha: 1)

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

    // The crossing: a half-circle arc from one shore to the other, lit from within.
    let c = CGPoint(x: 512, y: 506)
    let r: CGFloat = 250
    let glow = CGGradient(colorsSpace: srgb, colors: [
        violet.withAlphaComponent(0.42).cgColor, violet.withAlphaComponent(0).cgColor] as CFArray, locations: [0, 1])!
    let glowAt = CGPoint(x: c.x, y: c.y + r * 0.55)
    ctx.drawRadialGradient(glow, startCenter: glowAt, startRadius: 0, endCenter: glowAt, endRadius: 330, options: [])
    ctx.setLineCap(.round)
    ctx.setStrokeColor(violet.cgColor)
    ctx.setLineWidth(44)
    ctx.addArc(center: c, radius: r, startAngle: .pi, endAngle: 0, clockwise: true)
    ctx.strokePath()

    // The shores: a white dot at each end of the arc.
    ctx.setFillColor(NSColor(white: 1, alpha: 0.95).cgColor)
    for x in [c.x - r, c.x + r] {
        ctx.fillEllipse(in: CGRect(x: x - 46, y: c.y - 46, width: 92, height: 92))
    }

    // The water: two gentle waves under the crossing, the lower one fainter.
    func wave(y: CGFloat, alpha: CGFloat, width: CGFloat) {
        let (x0, x1): (CGFloat, CGFloat) = (270, 754)
        let amp: CGFloat = 20, periods: CGFloat = 2.5
        ctx.beginPath()
        for i in 0...96 {
            let t = CGFloat(i) / 96
            let p = CGPoint(x: x0 + (x1 - x0) * t, y: y + amp * sin(t * periods * 2 * .pi))
            if i == 0 { ctx.move(to: p) } else { ctx.addLine(to: p) }
        }
        ctx.setStrokeColor(NSColor(white: 1, alpha: alpha).cgColor)
        ctx.setLineWidth(width)
        ctx.setLineJoin(.round)
        ctx.strokePath()
    }
    wave(y: 366, alpha: 0.95, width: 38)
    wave(y: 272, alpha: 0.5, width: 30)

    ctx.restoreGState()
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
// The iconset is an intermediate build product of this script.
try? fm.removeItem(at: set)
print("Wrote assets/AppIcon.icns and assets/logo.png")
