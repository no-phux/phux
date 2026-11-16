"""Execute the shipped cadence policy, not a Python copy of its decisions."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from test_ios_build import build


@unittest.skipUnless(sys.platform == "darwin", "Swift host execution needs Xcode")
class FrameCadenceTests(unittest.TestCase):
    def test_failed_drawables_keep_retrying_and_preserve_earlier_deadlines(self):
        source = Path(__file__).resolve().parents[1] / "ios/Sources/FrameCadence.swift"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            main = root / "main.swift"
            main.write_text("""
import Foundation
var cadence = FrameCadence()
precondition(cadence.acceptsWake(timerScheduled: true))
for expected in [0.05, 0.1, 0.2, 0.25, 0.25, 0.25, 0.25, 0.25] {
    precondition(cadence.nextWake(drew: false, after: nil) == expected)
    // Ordinary input/network wakes cannot turn a pending retry into 120 Hz
    // acquisition attempts, but a cancelled retry may be rearmed.
    precondition(!cadence.acceptsWake(timerScheduled: true))
    precondition(cadence.acceptsWake(timerScheduled: false))
}
precondition(cadence.nextWake(drew: false, after: 0.01) == 0.01)
precondition(cadence.nextWake(drew: false, after: 10) == 0.25)
precondition(cadence.nextWake(drew: true, after: nil) == nil)
precondition(cadence.nextWake(drew: false, after: nil) == 0.05)
cadence.reset()
precondition(cadence.acceptsWake(timerScheduled: true))
precondition(cadence.nextWake(drew: false, after: .infinity) == 0.05)
precondition(cadence.nextWake(drew: true, after: .nan) == nil)
precondition(cadence.nextWake(drew: true, after: -1) == 0.001)
precondition(cadence.nextWake(drew: true, after: 2) == 2)
""")
            compiled = subprocess.run(["/usr/bin/xcrun", "--sdk", "macosx", "swiftc", "-swift-version", "6",
                            "-warnings-as-errors", str(source), str(main), "-o", str(root / "cadence")],
                           env=build.apple_environment(root), capture_output=True, text=True, timeout=120)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            subprocess.run([str(root / "cadence")], check=True, timeout=10)


if __name__ == "__main__":
    unittest.main()
