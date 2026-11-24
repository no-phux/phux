"""Native Liquid Glass qualification on the runner's own simulator.

After the terminal evidence, the runner posts a Darwin notification inside
its simulator. The app's probe records the top bar's UIKit glass controls,
taps the real glass back button, and reports once the Sessions page has
removed the covered bar's glass and shown its own glass dock.
"""
import shutil
import time

SIGNAL = "sh.phux.rune.qualification.glass"
TOP_BAR = ("phone-back", "phone-place", "phone-news", "phone-more")
DOCK = "phone-session-new"


def _controls(controls, phase):
    if not isinstance(controls, list) or not all(isinstance(c, dict) for c in controls):
        raise ValueError(f"invalid {phase} Liquid Glass controls")
    for control in controls:
        frame = control.get("frame")
        if (not isinstance(control.get("id"), str) or not isinstance(control.get("label"), str)
                or not control["label"] or control.get("shown") is not True
                or not isinstance(frame, list) or len(frame) != 4
                or not all(isinstance(v, (int, float)) for v in frame)
                or frame[2] < 44 or frame[3] < 44):
            raise ValueError(f"invalid {phase} Liquid Glass control: {control!r}")
    return {c["id"]: c for c in controls}


def validate(glass):
    """The terminal's top bar was native glass; tapping its real back button
    opened Sessions, whose page hid that bar's glass and showed the dock's."""
    if glass.get("schema") != 1 or glass.get("tapped") != "phone-back":
        raise ValueError("invalid Liquid Glass qualification")
    terminal = _controls(glass.get("terminal"), "terminal")
    if not set(TOP_BAR).issubset(terminal) or not all(terminal[i]["enabled"] is True for i in TOP_BAR):
        raise ValueError(f"terminal top bar is not native glass: {sorted(terminal)}")
    sessions = _controls(glass.get("sessions"), "sessions")
    if DOCK not in sessions or set(TOP_BAR) & set(sessions):
        raise ValueError(f"Sessions page glass is wrong: {sorted(sessions)}")
    if sessions[DOCK]["enabled"] is not True:
        raise ValueError("the connected Sessions dock is disabled")


def qualify(lane, pid):
    """Drive the probe's glass step on `lane`'s simulator and keep its report
    and a screenshot of the Sessions page."""
    if lane.documents is None:
        raise ValueError("app data container was not resolved before launch")
    source = lane.documents / "rune-glass.json"
    source.unlink(missing_ok=True)
    lane.simctl("spawn", lane.udid, "notifyutil", "-p", SIGNAL)
    glass = lane.await_scene_report(source, pid, "native Liquid Glass controls were not qualified")
    shutil.copy2(source, lane.out / source.name)
    validate(glass)
    # The page's entrance settles before its picture is evidence.
    time.sleep(2)
    lane.require_alive(pid)
    screenshot = lane.out / "ios-glass-sessions.png"
    # Read-only: a stalled screenshot call may be retried (qualify.QUERY_ATTEMPTS).
    lane.simctl("io", lane.udid, "screenshot", screenshot, attempts=3)
    if not screenshot.stat().st_size:
        raise ValueError("empty Liquid Glass screenshot")
    return glass
