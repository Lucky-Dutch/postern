// Draws the README banner: the icon, the name, a tagline, and technology chips
// on a rounded card. Run through scripts/make-artwork.sh.
//
//   swift scripts/banner.swift <dark|light> <icon.png> <output.png>

import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

let arguments = CommandLine.arguments
guard arguments.count == 4 else { fatalError("usage: banner.swift <dark|light> <icon.png> <output.png>") }
let dark = arguments[1] == "dark"
let width = 1760
let height = 560

func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: alpha)
}

let space = CGColorSpace(name: CGColorSpace.sRGB)!
guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                              space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
else { fatalError("Could not create the drawing context") }
// Native coordinates: the origin is at the bottom left.

let card = CGPath(roundedRect: CGRect(x: 0, y: 0, width: width, height: height),
                  cornerWidth: 44, cornerHeight: 44, transform: nil)
context.addPath(card)
context.clip()

let background = dark ? [rgb(0x0B1626), rgb(0x10264A), rgb(0x0E1E36)] : [rgb(0xF7FAFE), rgb(0xE9F0FA), rgb(0xDDE8F6)]
let gradient = CGGradient(colorsSpace: space, colors: background as CFArray, locations: [0, 0.6, 1])!
context.drawLinearGradient(gradient, start: CGPoint(x: 0, y: CGFloat(height)), end: CGPoint(x: CGFloat(width), y: 0), options: [])

func glow(_ center: CGPoint, _ radius: CGFloat, _ color: CGColor) {
    let colors = [color, color.copy(alpha: 0)!] as CFArray
    let radial = CGGradient(colorsSpace: space, colors: colors, locations: [0, 1])!
    context.drawRadialGradient(radial, startCenter: center, startRadius: 0, endCenter: center, endRadius: radius, options: [])
}
glow(CGPoint(x: 330, y: 300), 520, dark ? rgb(0x3B82F6, 0.38) : rgb(0x60A5FA, 0.30))
glow(CGPoint(x: 1560, y: 60), 560, dark ? rgb(0x14B8A6, 0.22) : rgb(0x2DD4BF, 0.18))
glow(CGPoint(x: 1300, y: 560), 420, dark ? rgb(0x8B5CF6, 0.16) : rgb(0xA78BFA, 0.14))

// A faint grid, like the cells of a table, fading toward the left.
context.saveGState()
context.setLineWidth(1.5)
let step: CGFloat = 44
var x: CGFloat = 0
while x <= CGFloat(width) {
    let alpha = (dark ? 0.07 : 0.08) * min(1, x / CGFloat(width) * 1.6)
    context.setStrokeColor(dark ? rgb(0xFFFFFF, alpha) : rgb(0x1E3A5F, alpha))
    context.move(to: CGPoint(x: x, y: 0))
    context.addLine(to: CGPoint(x: x, y: CGFloat(height)))
    context.strokePath()
    x += step
}
var y: CGFloat = 0
while y <= CGFloat(height) {
    let segments = stride(from: CGFloat(0), to: CGFloat(width), by: step)
    for start in segments {
        let alpha = (dark ? 0.07 : 0.08) * min(1, start / CGFloat(width) * 1.6)
        context.setStrokeColor(dark ? rgb(0xFFFFFF, alpha) : rgb(0x1E3A5F, alpha))
        context.move(to: CGPoint(x: start, y: y))
        context.addLine(to: CGPoint(x: start + step, y: y))
        context.strokePath()
    }
    y += step
}
context.restoreGState()

// The icon.
guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: arguments[2]) as CFURL, nil),
      let icon = CGImageSourceCreateImageAtIndex(source, 0, nil)
else { fatalError("Could not read \(arguments[2])") }
context.interpolationQuality = .high
context.draw(icon, in: CGRect(x: 96, y: 60, width: 440, height: 440))

func font(_ size: CGFloat, bold: Bool) -> CTFont {
    CTFontCreateUIFontForLanguage(bold ? .emphasizedSystem : .system, size, nil)!
}

@discardableResult
func text(_ string: String, _ font: CTFont, _ color: CGColor, at point: CGPoint, kern: CGFloat = 0) -> CGFloat {
    let attributes: [NSAttributedString.Key: Any] = [
        NSAttributedString.Key(kCTFontAttributeName as String): font,
        NSAttributedString.Key(kCTForegroundColorAttributeName as String): color,
        NSAttributedString.Key(kCTKernAttributeName as String): kern,
    ]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: string, attributes: attributes))
    context.textPosition = point
    CTLineDraw(line, context)
    return CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
}

let textLeft: CGFloat = 600
text("Postern", font(150, bold: true), dark ? rgb(0xFFFFFF) : rgb(0x0F2540), at: CGPoint(x: textLeft, y: 300), kern: -3)
text("A fast PostgreSQL client for macOS, written in Rust.", font(44, bold: false),
     dark ? rgb(0xB4CBEA) : rgb(0x35557F), at: CGPoint(x: textLeft + 6, y: 228))

// Technology chips.
var chipX = textLeft + 4
for label in ["Rust", "egui", "Tokio", "PostgreSQL 14+", "Apple Silicon"] {
    let chipFont = font(28, bold: true)
    let attributes: [NSAttributedString.Key: Any] = [NSAttributedString.Key(kCTFontAttributeName as String): chipFont]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: label, attributes: attributes))
    let textWidth = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
    let chip = CGRect(x: chipX, y: 104, width: textWidth + 44, height: 56)
    let path = CGPath(roundedRect: chip, cornerWidth: 28, cornerHeight: 28, transform: nil)
    context.addPath(path)
    context.setFillColor(dark ? rgb(0xFFFFFF, 0.08) : rgb(0xFFFFFF, 0.75))
    context.fillPath()
    context.addPath(path)
    context.setStrokeColor(dark ? rgb(0x9CC3FF, 0.35) : rgb(0x2A5482, 0.25))
    context.setLineWidth(2)
    context.strokePath()
    text(label, chipFont, dark ? rgb(0xDCE8F8) : rgb(0x1F3F66), at: CGPoint(x: chipX + 22, y: 122))
    chipX += textWidth + 60
}

guard let image = context.makeImage(),
      let destination = CGImageDestinationCreateWithURL(URL(fileURLWithPath: arguments[3]) as CFURL,
                                                        UTType.png.identifier as CFString, 1, nil)
else { fatalError("Could not create \(arguments[3])") }
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else { fatalError("Could not write \(arguments[3])") }
print("Wrote \(arguments[3])")
