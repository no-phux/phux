// Key to pixels for one app's window: posts KEYS keystrokes GAP_MS apart to
// the process PID (a letter, then Backspace, as post-keys.swift does) and
// watches that window's pixels with ScreenCaptureKit at the display's refresh
// rate. Each key's latency runs from the post to the display time of the
// first captured frame that differs from the one before the key. Prints the
// median and p99.
//
// Keys go to the process with CGEvent.postToPid, never to whatever window is
// focused. The window is brought to the front over Accessibility, since an
// app handles keys only in its key window and some draw nothing while
// covered. Turn off cursor blinking in the target: a blink is a pixel change.
//
// What it measures ends where the compositor shows a frame; the display's
// scanout and panel response come after and are the same for any app on the
// same display, so compare apps on one display, and read every number as
// rounded up to its refresh interval.
//
// Needs the Screen Recording and Accessibility permissions for the app that
// runs it (System Settings > Privacy & Security).
//   swiftc -O key-photon.swift -o key-photon && ./key-photon PID [KEYS] [GAP_MS]
import ApplicationServices
import CoreMedia
import Foundation
import ScreenCaptureKit

func fail(_ words: String) -> Never {
    FileHandle.standardError.write(Data("key-photon: \(words)\n".utf8))
    exit(1)
}

let args = CommandLine.arguments
guard args.count >= 2, let pid = pid_t(args[1]) else {
    fail("usage: key-photon PID [KEYS] [GAP_MS]")
}
let keys = args.count > 2 ? Int(args[2])! : 100
let gap = (args.count > 3 ? Double(args[3])! : 250) / 1000
if !CGPreflightScreenCaptureAccess() {
    fail("needs Screen Recording (System Settings > Privacy & Security > Screen Recording)")
}
if !CGPreflightPostEventAccess() || !AXIsProcessTrusted() {
    fail("needs Accessibility (System Settings > Privacy & Security > Accessibility)")
}

let session = CGSessionCopyCurrentDictionary() as? [String: Any] ?? [:]
if session["CGSSessionScreenIsLocked"] as? Bool == true {
    fail("the screen is locked: unlock it, and keep the display awake for the run")
}

var timebase = mach_timebase_info_data_t()
mach_timebase_info(&timebase)
func millis(_ ticks: UInt64) -> Double {
    Double(ticks) * Double(timebase.numer) / Double(timebase.denom) / 1e6
}

// To the front, so the window is the app's key window and uncovered.
let app = AXUIElementCreateApplication(pid)
AXUIElementSetAttributeValue(app, kAXFrontmostAttribute as CFString, kCFBooleanTrue)
var main: CFTypeRef?
if AXUIElementCopyAttributeValue(app, kAXMainWindowAttribute as CFString, &main) == .success {
    AXUIElementPerformAction(main as! AXUIElement, kAXRaiseAction as CFString)
}
usleep(500_000)

let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
func area(_ r: CGRect) -> CGFloat { r.isNull ? 0 : r.width * r.height }
guard let window = content.windows
    .filter({ $0.owningApplication?.processID == pid && $0.windowLayer == 0 })
    .max(by: { area($0.frame) < area($1.frame) })
else { fail("process \(pid) has no window on screen") }
guard let display = content.displays
    .max(by: { area($0.frame.intersection(window.frame)) < area($1.frame.intersection(window.frame)) })
else { fail("no active display: is it asleep?") }
let hz = CGDisplayCopyDisplayMode(display.displayID).map(\.refreshRate).flatMap { $0 > 0 ? $0 : nil } ?? 60

let filter = SCContentFilter(display: display, including: [window])
let scale = CGFloat(filter.pointPixelScale)
let config = SCStreamConfiguration()
config.sourceRect = window.frame.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
config.width = Int(window.frame.width * scale)
config.height = Int(window.frame.height * scale)
config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(hz.rounded()))
config.pixelFormat = kCVPixelFormatType_32BGRA
config.showsCursor = false
config.queueDepth = 8

/// The newest frame's pixels, and the key waiting for them to change.
final class Watch: NSObject, SCStreamOutput, @unchecked Sendable {
    let lock = NSLock()
    let changed = DispatchSemaphore(value: 0)
    var pixels = Data()
    /// The post's time and the pixels before it, while a key waits.
    var armed: (at: UInt64, before: Data)?
    var latency: UInt64 = 0

    func stream(_ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let infos = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false)
                as? [[SCStreamFrameInfo: Any]],
              let info = infos.first,
              (info[.status] as? Int).flatMap(SCFrameStatus.init) == .complete,
              let shown = info[.displayTime] as? UInt64,
              let image = CMSampleBufferGetImageBuffer(buffer)
        else { return }
        // Each row's pixels only: a row's padding is not the picture.
        CVPixelBufferLockBaseAddress(image, .readOnly)
        let base = CVPixelBufferGetBaseAddress(image)!, stride = CVPixelBufferGetBytesPerRow(image)
        let row = CVPixelBufferGetWidth(image) * 4
        var bytes = Data(capacity: row * CVPixelBufferGetHeight(image))
        for y in 0..<CVPixelBufferGetHeight(image) {
            bytes.append(base.advanced(by: y * stride).assumingMemoryBound(to: UInt8.self), count: row)
        }
        CVPixelBufferUnlockBaseAddress(image, .readOnly)
        lock.lock()
        defer { lock.unlock() }
        pixels = bytes
        guard let key = armed else { return }
        if shown <= key.at {
            // Shown before the key: it is the new "before".
            armed = (key.at, bytes)
        } else if bytes != key.before {
            latency = shown - key.at
            armed = nil
            changed.signal()
        }
    }
}

/// Post the keys and time each one's change; the samples in ms and the keys
/// that changed nothing within a second.
func measure(_ stream: SCStream, _ watch: Watch) -> ([Double], Int) {
    let started = DispatchSemaphore(value: 0)
    stream.startCapture { error in
        if let error { fail("capture: \(error.localizedDescription)") }
        started.signal()
    }
    started.wait()
    usleep(1_000_000)
    var samples: [Double] = []
    var missed = 0
    for k in 0..<keys {
        usleep(useconds_t(gap * 1e6))
        let code: CGKeyCode = k % 2 == 0 ? 0 : 51
        let down = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true)!
        let up = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false)!
        watch.lock.lock()
        watch.armed = (mach_absolute_time(), watch.pixels)
        watch.lock.unlock()
        down.postToPid(pid)
        up.postToPid(pid)
        let answered = watch.changed.wait(timeout: .now() + 1) == .success
        watch.lock.lock()
        if answered || watch.armed == nil {
            // A change that landed just past the timeout still counts.
            if !answered { watch.changed.wait() }
            samples.append(millis(watch.latency))
        } else {
            watch.armed = nil
            missed += 1
        }
        watch.lock.unlock()
    }
    stream.stopCapture()
    return (samples, missed)
}

let watch = Watch()
let stream = SCStream(filter: filter, configuration: config, delegate: nil)
try stream.addStreamOutput(watch, type: .screen, sampleHandlerQueue: DispatchQueue(label: "frames"))
var (samples, missed) = measure(stream, watch)

guard !samples.isEmpty else { fail("no key changed the window's pixels") }
samples.sort()
func pct(_ p: Double) -> Double { samples[min(samples.count - 1, Int(Double(samples.count) * p))] }
let name = window.owningApplication?.applicationName ?? "pid \(pid)"
print(String(format: "%@: key to pixels over %d keys (%d missed), display %u at %.0f Hz: "
    + "p50 %.1f ms, p99 %.1f ms, min %.1f ms, max %.1f ms",
    name, samples.count, missed, display.displayID, hz, pct(0.5), pct(0.99), samples[0], samples.last!))
