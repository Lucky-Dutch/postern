// Puts a screenshot of the application's framebuffer in a macOS-style window
// frame with a title bar and a shadow, for the README. Run through
// scripts/make-artwork.sh.
//
//   swift scripts/frame.swift <dark|light> <screenshot.png> <output.png>

import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

let arguments = CommandLine.arguments
guard arguments.count == 4 else { fatalError("usage: frame.swift <dark|light> <screenshot.png> <output.png>") }
let dark = arguments[1] == "dark"

func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: alpha)
}

guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: arguments[2]) as CFURL, nil),
      let screenshot = CGImageSourceCreateImageAtIndex(source, 0, nil)
else { fatalError("Could not read \(arguments[2])") }

// Screenshots are captured at 2x; the frame uses the same scale.
let shotWidth = CGFloat(screenshot.width)
let shotHeight = CGFloat(screenshot.height)
let titleBar: CGFloat = 64
let margin: CGFloat = 96
let width = Int(shotWidth + margin * 2)
let height = Int(shotHeight + titleBar + margin * 2)

let space = CGColorSpace(name: CGColorSpace.sRGB)!
guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                              space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
else { fatalError("Could not create the drawing context") }

let window = CGRect(x: margin, y: margin, width: shotWidth, height: shotHeight + titleBar)
let outline = CGPath(roundedRect: window, cornerWidth: 26, cornerHeight: 26, transform: nil)

context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -28), blur: 70, color: rgb(0x000000, dark ? 0.55 : 0.28))
context.addPath(outline)
context.setFillColor(dark ? rgb(0x2B2E34) : rgb(0xECECEE))
context.fillPath()
context.restoreGState()

context.saveGState()
context.addPath(outline)
context.clip()
// Title bar.
let bar = CGRect(x: window.minX, y: window.maxY - titleBar, width: window.width, height: titleBar)
let barColors = dark ? [rgb(0x34373E), rgb(0x2B2E34)] : [rgb(0xF4F4F6), rgb(0xE4E4E7)]
let barGradient = CGGradient(colorsSpace: space, colors: barColors as CFArray, locations: nil)!
context.drawLinearGradient(barGradient, start: CGPoint(x: 0, y: bar.maxY), end: CGPoint(x: 0, y: bar.minY), options: [])
for (index, color) in [rgb(0xFF5F57), rgb(0xFEBC2E), rgb(0x28C840)].enumerated() {
    let center = CGPoint(x: bar.minX + 40 + CGFloat(index) * 40, y: bar.midY)
    context.addEllipse(in: CGRect(x: center.x - 13, y: center.y - 13, width: 26, height: 26))
    context.setFillColor(color)
    context.fillPath()
}
let title = NSAttributedString(string: "Postern", attributes: [
    NSAttributedString.Key(kCTFontAttributeName as String): CTFontCreateUIFontForLanguage(.emphasizedSystem, 26, nil)!,
    NSAttributedString.Key(kCTForegroundColorAttributeName as String): dark ? rgb(0xC9CCD2) : rgb(0x4A4D55),
])
let line = CTLineCreateWithAttributedString(title)
let titleWidth = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
context.textPosition = CGPoint(x: bar.midX - titleWidth / 2, y: bar.midY - 9)
CTLineDraw(line, context)
// The application's framebuffer.
context.interpolationQuality = .high
context.draw(screenshot, in: CGRect(x: window.minX, y: window.minY, width: shotWidth, height: shotHeight))
context.restoreGState()

// A hairline border.
context.addPath(outline)
context.setStrokeColor(dark ? rgb(0xFFFFFF, 0.14) : rgb(0x000000, 0.16))
context.setLineWidth(2)
context.strokePath()

guard let image = context.makeImage(),
      let destination = CGImageDestinationCreateWithURL(URL(fileURLWithPath: arguments[3]) as CFURL,
                                                        UTType.png.identifier as CFString, 1, nil)
else { fatalError("Could not create \(arguments[3])") }
CGImageDestinationAddImage(destination, image, nil)
guard CGImageDestinationFinalize(destination) else { fatalError("Could not write \(arguments[3])") }
print("Wrote \(arguments[3])")
