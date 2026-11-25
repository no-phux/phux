"""OS lifecycle half of iOS qualification: background/foreground of the real
UIKit scene, and a relaunch that must reconnect from the saved identity alone.
Bound onto qualify.Qualification; qualify.py supplies the shared names."""

import re
import shutil

SESSION = INPUT_GATE = MARKER = QUERY_ATTEMPTS = marker_command = None


def bind(**names):
    """Take qualify.py's constants without importing it back (no package)."""
    globals().update(names)


def cycle_scene(self, documents, pid, prefix=""):
    # Drive the actual UIKit scene to background/foreground on our own
    # simulator. An in-app state toggle is not OS lifecycle evidence.
    background = documents / "rune-background.json"
    background.unlink(missing_ok=True)
    # Home is already running; cold Settings startup can stall its launch RPC.
    # Home twice is still Home: a stalled call may be retried.
    self.simctl("launch", self.udid, "com.apple.springboard", attempts=QUERY_ATTEMPTS)
    self.await_scene_report(background, pid, "native scene did not acknowledge OS backgrounding")
    shutil.copy2(background, self.out / (prefix + background.name))
    self.require_alive(pid)
    lifecycle = documents / "rune-lifecycle.json"
    lifecycle.unlink(missing_ok=True)
    self.simctl("launch", self.udid, self.bundle_id)
    resumed = self.await_scene_report(lifecycle, pid, "no presented frame after native background/foreground")
    if not resumed.get("marker_seen") or resumed.get("resumed_frames", 0) <= resumed.get("background_frames", 0):
        raise ValueError("native lifecycle did not resume the same live terminal")
    shutil.copy2(lifecycle, self.out / (prefix + lifecycle.name))

def resume_identity(self, original):
    # New process, no ticket/configuration launch argument: only saved
    # public preferences plus Keychain token and held key may reconnect.
    self.finish_connection_diagnostics()
    self.simctl("terminate", self.udid, self.bundle_id, timeout=60)
    self.preserve_connection_diagnostics(prefixes=("",))
    for name in ("rune-qualification.json", "rune-tree.json", INPUT_GATE):
        (self.documents / name).unlink(missing_ok=True)
    marker = MARKER + "_RESUMED"
    self.diagnostic_prefixes.append("resume-")
    launched = self.simctl("launch", self.udid, self.bundle_id, "--qualification-resume",
        "--qualification-session", SESSION, "--qualification-input", marker_command(marker),
        "--qualification-marker", marker, timeout=120)
    match = re.search(rf"^{re.escape(self.bundle_id)}: ([0-9]+)$", launched, re.MULTILINE)
    if not match:
        raise ValueError("simulator did not report the resumed app PID")
    self.diagnostic_pid = int(match[1])
    report = self.collect_report(self.diagnostic_pid, resumed=True)
    if report["key_id"] != original["key_id"]:
        raise ValueError("relaunch did not use the same saved device identity")
    return report
