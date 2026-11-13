// Posts KEYS keystrokes GAP_MS apart to the process PID, a letter then
// Backspace, as a keyboard would: rune-echo.sh --window types into a real
// window with it. Posting input needs the Accessibility permission for the
// app that runs it (System Settings > Privacy & Security > Accessibility).
//   swiftc -O post-keys.swift -o post-keys && ./post-keys PID KEYS GAP_MS
import CoreGraphics
import Foundation
let args = CommandLine.arguments
let pid = pid_t(args[1])!, keys = Int(args[2])!, gap = Double(args[3])! / 1000
for k in 0..<keys {
    let code: CGKeyCode = k % 2 == 0 ? 0 : 51
    for down in [true, false] {
        CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)!.postToPid(pid)
    }
    Thread.sleep(forTimeInterval: gap)
}
