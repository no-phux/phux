// Installer chrome, not new logo geometry: composite the pinned upstream pixels.
import AppKit
import CoreServices

let wordmark = NSImage(contentsOfFile: CommandLine.arguments[1])!
let logo = NSImage(contentsOfFile: CommandLine.arguments[2])!
let output = CommandLine.arguments[3]

func text(_ value: String, x: CGFloat, y: CGFloat, size: CGFloat, color: NSColor) {
    (value as NSString).draw(at: NSPoint(x: x, y: y), withAttributes: [
        .font: NSFont.systemFont(ofSize: size, weight: .medium), .foregroundColor: color,
    ])
}

func png(_ name: String, size: NSSize, scale: Int, draw: () -> Void) throws {
    let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width) * scale,
        pixelsHigh: Int(size.height) * scale, bitsPerSample: 8, samplesPerPixel: 4,
        hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    bitmap.size = size
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
    NSGraphicsContext.current!.imageInterpolation = .high
    draw()
    NSGraphicsContext.restoreGraphicsState()
    try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: output + "/" + name))
}

let ink = NSColor(calibratedRed: 0.10, green: 0.12, blue: 0.14, alpha: 1)
let muted = NSColor(calibratedWhite: 0.38, alpha: 1)
let accent = NSColor(calibratedRed: 0.19, green: 0.44, blue: 0.38, alpha: 1)
for scale in [1, 2] {
    let suffix = scale == 1 ? "" : "@2x"
    try png("background\(suffix).png", size: NSSize(width: 720, height: 460), scale: scale) {
        NSColor(calibratedRed: 0.95, green: 0.94, blue: 0.91, alpha: 1).setFill()
        NSRect(x: 0, y: 0, width: 720, height: 460).fill()
        ink.setFill()
        NSRect(x: 0, y: 318, width: 720, height: 142).fill()
        wordmark.draw(in: NSRect(x: 30, y: 350, width: 216, height: 78))
        text("Your terminals. Your agents. Your machine.", x: 286, y: 383, size: 15,
            color: NSColor(calibratedWhite: 0.85, alpha: 1))
        text("Drag Phux to Applications", x: 248, y: 278, size: 18, color: ink)
        let arrow = NSBezierPath()
        arrow.move(to: NSPoint(x: 320, y: 212)); arrow.line(to: NSPoint(x: 400, y: 212))
        arrow.move(to: NSPoint(x: 384, y: 228)); arrow.line(to: NSPoint(x: 400, y: 212))
        arrow.line(to: NSPoint(x: 384, y: 196))
        arrow.lineWidth = 3; arrow.lineCapStyle = .round; arrow.lineJoinStyle = .round
        accent.setStroke(); arrow.stroke()
        text("Then open Phux from Applications and eject this disk.", x: 169, y: 120, size: 14, color: muted)
        text("Apple silicon  /  macOS 26+", x: 30, y: 22, size: 11, color: muted)
        text("phux.sh", x: 224, y: 22, size: 11, color: muted)
    }
}

// Keep the familiar disk silhouette; badge it with the unmodified release icon.
let disk = NSWorkspace.shared.icon(forFileType: NSFileTypeForHFSTypeCode(OSType(kGenericHardDiskIcon)))
try png("VolumeIcon.png", size: NSSize(width: 1024, height: 1024), scale: 1) {
    disk.draw(in: NSRect(x: 0, y: 0, width: 1024, height: 1024))
    logo.draw(in: NSRect(x: 264, y: 310, width: 496, height: 496))
}
