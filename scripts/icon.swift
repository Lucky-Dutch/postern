// Draws the Postern application icon: a woolly mammoth whose head is a database
// cylinder, on a rounded square. Run through scripts/make-icon.sh.
//
//   swift scripts/icon.swift <output.png>

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let size = 1024
let output = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon.png"

func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(
        srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
        green: CGFloat((hex >> 8) & 0xFF) / 255,
        blue: CGFloat(hex & 0xFF) / 255,
        alpha: alpha
    )
}

let space = CGColorSpace(name: CGColorSpace.sRGB)!
guard let context = CGContext(
    data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
    space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
) else { fatalError("Could not create the drawing context") }

// Draw with the origin at the top left.
context.translateBy(x: 0, y: CGFloat(size))
context.scaleBy(x: 1, y: -1)

func fill(_ path: CGPath, _ color: CGColor) {
    context.addPath(path)
    context.setFillColor(color)
    context.fillPath()
}

func linearFill(_ path: CGPath, _ colors: [CGColor], from start: CGPoint, to end: CGPoint) {
    context.saveGState()
    context.addPath(path)
    context.clip()
    let gradient = CGGradient(colorsSpace: space, colors: colors as CFArray, locations: nil)!
    context.drawLinearGradient(gradient, start: start, end: end, options: [])
    context.restoreGState()
}

func ellipse(_ cx: CGFloat, _ cy: CGFloat, _ rx: CGFloat, _ ry: CGFloat) -> CGPath {
    CGPath(ellipseIn: CGRect(x: cx - rx, y: cy - ry, width: rx * 2, height: ry * 2), transform: nil)
}

// Background: the macOS icon grid's rounded square.
let tile = CGPath(
    roundedRect: CGRect(x: 100, y: 100, width: 824, height: 824),
    cornerWidth: 185, cornerHeight: 185, transform: nil
)
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: 14), blur: 28, color: rgb(0x000000, 0.35))
fill(tile, rgb(0x1F4466))
context.restoreGState()
linearFill(tile, [rgb(0x4A7FB0), rgb(0x2A5482), rgb(0x173556)],
           from: CGPoint(x: 512, y: 100), to: CGPoint(x: 512, y: 924))
// A soft light from the top.
linearFill(tile, [rgb(0xFFFFFF, 0.16), rgb(0xFFFFFF, 0)],
           from: CGPoint(x: 512, y: 100), to: CGPoint(x: 512, y: 480))

let center: CGFloat = 512
let left: CGFloat = 330
let right: CGFloat = 694
let rx = (right - left) / 2
let ry: CGFloat = 62
let top: CGFloat = 395
let bottom: CGFloat = 655

let brown = rgb(0x8A5A3B)
let darkBrown = rgb(0x5C3721)
let fur = rgb(0x6E4329)
let ivory = rgb(0xF6EEDC)
let ivoryShade = rgb(0xD9CBB0)

context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: 18), blur: 30, color: rgb(0x000000, 0.35))

// Ears, behind the head.
for side: CGFloat in [-1, 1] {
    let ear = CGMutablePath()
    let x = center + side * (rx + 8)
    ear.move(to: CGPoint(x: x - side * 30, y: 430))
    ear.addCurve(to: CGPoint(x: x + side * 62, y: 520),
                 control1: CGPoint(x: x + side * 70, y: 400),
                 control2: CGPoint(x: x + side * 92, y: 470))
    ear.addCurve(to: CGPoint(x: x - side * 22, y: 590),
                 control1: CGPoint(x: x + side * 40, y: 580),
                 control2: CGPoint(x: x + side * 10, y: 600))
    ear.closeSubpath()
    fill(ear, darkBrown)
    let inner = CGMutablePath()
    inner.move(to: CGPoint(x: x + side * 4, y: 450))
    inner.addCurve(to: CGPoint(x: x + side * 48, y: 520),
                   control1: CGPoint(x: x + side * 52, y: 438),
                   control2: CGPoint(x: x + side * 66, y: 480))
    inner.addCurve(to: CGPoint(x: x + side * 4, y: 568),
                   control1: CGPoint(x: x + side * 30, y: 560),
                   control2: CGPoint(x: x + side * 14, y: 572))
    inner.closeSubpath()
    fill(inner, rgb(0x7A4B2E))
}

// The domed, woolly crown of a mammoth's head, rising behind the lid.
let crown = CGMutablePath()
crown.move(to: CGPoint(x: left + 18, y: top + 8))
crown.addCurve(to: CGPoint(x: center + 18, y: 222),
               control1: CGPoint(x: left + 10, y: 300),
               control2: CGPoint(x: center - 90, y: 222))
crown.addCurve(to: CGPoint(x: right - 18, y: top + 8),
               control1: CGPoint(x: center + 120, y: 222),
               control2: CGPoint(x: right - 6, y: 300))
crown.closeSubpath()
linearFill(crown, [rgb(0x7A4B2E), fur], from: CGPoint(x: 512, y: 222), to: CGPoint(x: 512, y: top))

// Tusks, emerging from under the head and curving out and up.
for side: CGFloat in [-1, 1] {
    let tusk = CGMutablePath()
    let base = center + side * 70
    tusk.move(to: CGPoint(x: base - side * 30, y: bottom - 40))
    tusk.addCurve(to: CGPoint(x: center + side * 300, y: 600),
                  control1: CGPoint(x: base - side * 20, y: 820),
                  control2: CGPoint(x: center + side * 292, y: 800))
    tusk.addCurve(to: CGPoint(x: base + side * 60, y: bottom - 40),
                  control1: CGPoint(x: center + side * 262, y: 745),
                  control2: CGPoint(x: base + side * 50, y: 760))
    tusk.closeSubpath()
    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: 8), blur: 14, color: rgb(0x000000, 0.3))
    fill(tusk, ivory)
    context.restoreGState()
    linearFill(tusk, [ivory, ivoryShade],
               from: CGPoint(x: center, y: bottom), to: CGPoint(x: center + side * 300, y: 780))
}

// The cylinder body.
let body = CGMutablePath()
let kappa: CGFloat = 0.5523
body.move(to: CGPoint(x: left, y: top))
body.addLine(to: CGPoint(x: left, y: bottom))
body.addCurve(to: CGPoint(x: center, y: bottom + ry),
              control1: CGPoint(x: left, y: bottom + ry * kappa),
              control2: CGPoint(x: center - rx * kappa, y: bottom + ry))
body.addCurve(to: CGPoint(x: right, y: bottom),
              control1: CGPoint(x: center + rx * kappa, y: bottom + ry),
              control2: CGPoint(x: right, y: bottom + ry * kappa))
body.addLine(to: CGPoint(x: right, y: top))
body.closeSubpath()
fill(body, brown)
context.restoreGState()
linearFill(body, [rgb(0x6B4129), rgb(0x9A6845), rgb(0xA87451), rgb(0x8A5A3B), rgb(0x5E3822)],
           from: CGPoint(x: left, y: 500), to: CGPoint(x: right, y: 500))

// Disk separators of the database cylinder.
context.setStrokeColor(rgb(0x4A2B18, 0.85))
context.setLineWidth(9)
context.setLineCap(.round)
for y: CGFloat in [top + 88, top + 176] {
    // A groove that meets the sides at a slant, like the cylinder's rim.
    let groove = CGMutablePath()
    groove.move(to: CGPoint(x: left - 6, y: y - 6))
    groove.addCurve(to: CGPoint(x: center, y: y + ry),
                    control1: CGPoint(x: left + rx * 0.22, y: y + ry * 0.72),
                    control2: CGPoint(x: center - rx * 0.55, y: y + ry))
    groove.addCurve(to: CGPoint(x: right + 6, y: y - 6),
                    control1: CGPoint(x: center + rx * 0.55, y: y + ry),
                    control2: CGPoint(x: right - rx * 0.22, y: y + ry * 0.72))
    context.saveGState()
    context.addPath(body)
    context.clip()
    context.addPath(groove)
    context.strokePath()
    context.restoreGState()
}

// The trunk, starting under the lid and curling at the tip.
let trunk = CGMutablePath()
trunk.move(to: CGPoint(x: center - 62, y: top + 40))
trunk.addCurve(to: CGPoint(x: center - 36, y: 600),
               control1: CGPoint(x: center - 60, y: top + 110),
               control2: CGPoint(x: center - 38, y: 540))
trunk.addCurve(to: CGPoint(x: center - 26, y: 800),
               control1: CGPoint(x: center - 34, y: 680),
               control2: CGPoint(x: center - 40, y: 760))
trunk.addCurve(to: CGPoint(x: center + 56, y: 792),
               control1: CGPoint(x: center - 8, y: 846),
               control2: CGPoint(x: center + 52, y: 842))
trunk.addCurve(to: CGPoint(x: center + 22, y: 752),
               control1: CGPoint(x: center + 60, y: 768),
               control2: CGPoint(x: center + 38, y: 752))
trunk.addCurve(to: CGPoint(x: center + 36, y: 600),
               control1: CGPoint(x: center + 30, y: 700),
               control2: CGPoint(x: center + 34, y: 650))
trunk.addCurve(to: CGPoint(x: center + 62, y: top + 40),
               control1: CGPoint(x: center + 38, y: 540),
               control2: CGPoint(x: center + 60, y: top + 110))
trunk.closeSubpath()
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: 6), blur: 12, color: rgb(0x000000, 0.35))
fill(trunk, brown)
context.restoreGState()
linearFill(trunk, [rgb(0x7A4B2E), rgb(0xA87451), rgb(0x7A4B2E)],
           from: CGPoint(x: center - 60, y: 600), to: CGPoint(x: center + 60, y: 600))
// Wrinkles across the trunk.
context.setStrokeColor(rgb(0x4A2B18, 0.55))
context.setLineWidth(6)
for (y, half) in [(CGFloat(620), CGFloat(30)), (660, 28), (700, 28), (738, 28)] {
    context.move(to: CGPoint(x: center - half, y: y))
    context.addQuadCurve(to: CGPoint(x: center + half, y: y), control: CGPoint(x: center, y: y + 11))
}
context.strokePath()

// The lid.
let lid = ellipse(center, top, rx, ry)
linearFill(lid, [rgb(0xC08A62), rgb(0xA06D49)],
           from: CGPoint(x: center, y: top - ry), to: CGPoint(x: center, y: top + ry))
context.addPath(lid)
context.setStrokeColor(rgb(0x5C3721))
context.setLineWidth(8)
context.strokePath()

// Strands of wool on the crown, following its curve.
context.saveGState()
context.addPath(crown)
context.clip()
context.setStrokeColor(rgb(0x4A2B18, 0.45))
context.setLineWidth(7)
for offset: CGFloat in [-110, -55, 0, 55, 110] {
    let start = CGPoint(x: center + offset * 0.95, y: top - ry + 12)
    let end = CGPoint(x: center + offset * 1.1, y: 262 + abs(offset) * 0.42)
    context.move(to: start)
    context.addQuadCurve(to: end, control: CGPoint(x: center + offset * 1.08 - 12, y: (start.y + end.y) / 2))
}
context.strokePath()
context.restoreGState()

// Eyes.
for x: CGFloat in [center - 82, center + 82] {
    fill(ellipse(x, top + 120, 15, 19), rgb(0x24140B))
    fill(ellipse(x + 5, top + 113, 5, 6), rgb(0xFFFFFF, 0.85))
}

guard let image = context.makeImage(),
      let destination = CGImageDestinationCreateWithURL(
          URL(fileURLWithPath: output) as CFURL, UTType.png.identifier as CFString, 1, nil)
else { fatalError("Could not create \(output)") }
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else { fatalError("Could not write \(output)") }
print("Wrote \(output)")
