"""Test-only process fixtures; importing this module runs no processes."""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

import qualify_wayland_ime as ime

DRIVER = Path(__file__).with_name("qualify_wayland_ime.py")

# Every executable, including the server CLI and app, is a local test fake.
# There is no route to an installed phux, production socket, or real display.
FAKE = r'''
import fcntl, json, os, signal, sys, time
from pathlib import Path
root = Path(os.environ["STATE"])
name = Path(sys.argv[0]).name
args = sys.argv[1:]
mode = os.environ["MODE"]
with (root / "calls").open("a") as out:
    out.write(json.dumps([name, *args]) + "\n")

def seats_reply():
    if mode == "malformed-seats":
        return "not seat JSON"
    countfile = root / "seat-count"
    count = int(countfile.read_text()) + 1 if countfile.exists() else 1
    countfile.write_text(str(count))
    capabilities = 2
    if mode == "late-seat":
        capabilities = 0 if count < 3 else 2
    if mode == "no-keyboard":
        capabilities = 0
    (root / "seat0-capabilities").write_text(str(capabilities))
    with (root / "wayland-fixture-seats.log").open("a") as out:
        out.write(json.dumps({"capabilities": capabilities,
                              "app_exists": (root / "rune-fake.pid").exists()}) + "\n")
    return json.dumps([{"name": "seat0", "capabilities": capabilities}])

if name in ("fcitx5", "rune-fake"):
    pidfile = root / (name + ".pid")
    pending = root / (name + ".pid.tmp")
    pending.write_text(str(os.getpid()))
    pending.replace(pidfile)
    assert os.environ["WAYLAND_DEBUG"] == "1"
    if mode == "dead-" + name:
        sys.exit(7)
    if name == "fcitx5":
        assert not args, "fcitx must remain foreground"
    else:
        assert args == ["--socket", "fake.socket", "--session", "fresh-ime"]
        assert int((root / "seat0-capabilities").read_text()) & 2, "app spawned without keyboard"
        # The app and the fake keyboard both emit fixture protocol records.
        fcntl.fcntl(1, fcntl.F_SETFL, fcntl.fcntl(1, fcntl.F_GETFL) | os.O_APPEND)
        print("[1.0] zwp_text_input_v3@12.enter(wl_surface@4)", flush=True)
        print("[1.1] -> zwp_text_input_v3@12.enable()", flush=True)
    # Continuous cursor traffic must never become a quiet-frame readiness gate.
    while True:
        if name == "rune-fake":
            print("[2.0] -> zwp_text_input_v3@12.set_cursor_rectangle(1, 2, 3, 4)", flush=True)
            print("[2.1] -> zwp_text_input_v3@12.commit()", flush=True)
        time.sleep(0.02)
elif name == "dbus-send":
    method = args[-2] if args[-1].startswith("string:") else args[-1]
    if method.endswith("NameHasOwner"):
        ready = (root / "fcitx5.pid").exists() and mode != "no-bus"
        print("boolean " + ("true" if ready else "false"))
    elif method.endswith("GetNameOwner"):
        print('string ":1.0"')
    elif method.endswith("GetConnectionUnixProcessID"):
        pid = int((root / "fcitx5.pid").read_text())
        print("uint32", pid + (1 if mode == "wrong-bus-pid" else 0))
    else:
        focus = 0 if mode == "no-context" else 1
        frontend = "x11" if mode == "wrong-frontend" else "wayland_v2"
        if mode == "split-context":
            print("program:sh.phux.rune frontend:wayland_v2 cap:52 focus:0")
            print("program:other frontend:wayland_v2 cap:52 focus:1")
        else:
            print(f'  string "IC program:sh.phux.rune frontend:{frontend} cap:52 focus:{focus}"')
elif name == "swaymsg":
    if args == ["-r", "-t", "get_seats"]:
        print(seats_reply())
        sys.exit(0)
    if mode == "tree-command-fails":
        sys.exit(6)
    if mode == "hung-tree":
        time.sleep(20)
    pidfile = root / "rune-fake.pid"
    pid = int(pidfile.read_text()) if pidfile.exists() else 0
    stale = {"pid": pid + 1, "app_id": "sh.phux.rune", "focused": True}
    node = {"pid": pid, "app_id": "sh.phux.rune", "focused": mode != "wrong-focus"}
    if mode == "wrong-focus":
        print(json.dumps({"nodes": [node, stale]}))
    elif mode == "wrong-app-id":
        print(json.dumps({**node, "app_id": "other.app"}))
    else:
        print(json.dumps({"floating_nodes": [node]}))
elif name == "fcitx5-remote":
    if mode == "activation-fails" and args == ["-o"]:
        sys.exit(4)
    if args == ["-n"]:
        print("keyboard-us" if mode == "profile-only" else "pinyin")
    elif not args:
        print("1" if mode == "inactive" else "2")
elif name == "wtype":
    (root / "injected").write_text("once")
    if mode == "typing-fails":
        sys.exit(5)
    if mode == "app-dies-during-composition":
        os.kill(int((root / "rune-fake.pid").read_text()), signal.SIGTERM)
        time.sleep(0.05)
    with (root / "wayland-ime-app.log").open("a") as out:
        # Segmentation is intentionally unlike the historical "ni hao".
        out.write('[3.0] zwp_text_input_v3@12.preedit_string("ni’hao", 0, 6)\n')
        if mode != "no-commit":
            out.write('[3.1] zwp_text_input_v3@12.commit_string("你好")\n')
        out.write('[3.2] zwp_text_input_v3@12.done(47)\n')
elif name == "phux-fake":
    assert args == ["--socket", "fake.socket", "snapshot", "fresh-ime"]
    injected = (root / "injected").exists()
    echo = "你好" if injected or mode == "stale-session" else ""
    if injected:
        echo = {"plain-ascii": "nihao", "wrong-chinese": "你号",
                "no-echo": "", "empty-snapshot": ""}.get(mode, echo)
    if mode != "empty-snapshot" or not injected:
        print("│$ " + echo)
        print("│full snapshot row beyond the old four-row truncation\n" * 12, end="")
elif name == "grim":
    Path(args[-1]).write_bytes(b"fake fixture, not an app screenshot")
'''


class WaylandImeFixture:
    """Process fixture and evidence assertions shared by the IME test cases."""

    def run_driver(self, mode="success"):
        with tempfile.TemporaryDirectory(prefix="wayland-ime-", dir=os.environ.get("TMPDIR")) as directory:
            root = Path(directory)
            fake = root / "fake"
            fake.write_text(f"#!{sys.executable}\n" + FAKE)
            fake.chmod(0o755)
            names = ("fcitx5", "rune-fake", "dbus-send", "swaymsg",
                     "fcitx5-remote", "wtype", "phux-fake", "grim")
            for name in names:
                (root / name).symlink_to(fake)
            env = {"PATH": f"{root}:/usr/bin:/bin", "HOME": directory,
                   "STATE": directory, "MODE": mode, "LANG": "C.UTF-8"}
            try:
                result = subprocess.run(
                    [sys.executable, "-B", __file__, directory,
                     str(root / "rune-fake"), str(root / "phux-fake"),
                     "fake.socket", "fresh-ime"],
                    env=env, text=True, capture_output=True, timeout=60,
                )
                calls_path = root / "calls"
                self.assertTrue(calls_path.is_file(), result.stderr)
                calls = [json.loads(line) for line in calls_path.read_text().splitlines()]
                stages = [json.loads(line) for line in
                          (root / "wayland-ime-stages.log").read_text().splitlines()]
                files = {p.name: p.read_text(errors="replace") for p in root.glob("wayland-*")}
                for event in stages:
                    if event["stage"] == "process-start":
                        with self.assertRaises(ProcessLookupError, msg="owned children must be reaped"):
                            os.kill(event["pid"], 0)
                return result, calls, stages, files
            finally:
                # Only fixture-owned PIDs, and only if a failed test left them.
                for pidfile in root.glob("*.pid"):
                    try:
                        os.kill(int(pidfile.read_text()), signal.SIGTERM)
                    except ProcessLookupError:
                        pass

    def assert_failure_evidence(self, stages, files):
        self.assertTrue(any(s["stage"] == "failure-liveness" for s in stages))
        for artifact in ("seats-failure.json", "tree-failure.json", "context-failure.log",
                         "engine-failure.log", "state-failure.log", "terminal-failure.log"):
            self.assert_captured_observation("wayland-ime-" + artifact, stages, files)
        self.assertIn("full snapshot row", files["wayland-ime-terminal-failure.log"])
        self.assertIn("wayland-ime-failed.png", files)
        self.assertTrue(all("time" in s and "elapsed" in s for s in stages))

    def assert_captured_observation(self, artifact, stages, files):
        if artifact in files:
            return
        self.assertIn(artifact + ".failed.stdout.log", files)
        self.assertIn(artifact + ".failed.stderr.log", files)
        events = [(s["stage"], s.get("artifact")) for s in stages]
        self.assertIn(("command-observation-failed", artifact), events)
        self.assertIn(("evidence-error", artifact), events)


class CommandObservationFixture:
    """Mock child writes and observation assertions for command-boundary tests."""

    def record_event(self, stage, **fields):
        self.events.append({"stage": stage, **fields})

    def seed_previous(self):
        (self.root / self.artifact).write_bytes(self.previous_out)
        (self.root / (self.artifact + ".stderr.log")).write_bytes(self.previous_err)

    def assert_previous(self):
        self.assertEqual((self.root / self.artifact).read_bytes(), self.previous_out)
        self.assertEqual((self.root / (self.artifact + ".stderr.log")).read_bytes(), self.previous_err)

    def complete_command(self, args, stdout, stderr, **options):
        # Write as a child process does: no dependency on text/binary wrappers.
        os.write(stdout.fileno(), self.next_out)
        os.write(stderr.fileno(), self.next_err)
        if self.failure is not None:
            raise self.failure
        return subprocess.CompletedProcess(args, self.returncode)

    def assert_failed_attempt(self, status):
        stdout = self.artifact + ".failed.stdout.log"
        stderr = self.artifact + ".failed.stderr.log"
        self.assertEqual((self.root / stdout).read_bytes(), self.next_out)
        self.assertEqual((self.root / stderr).read_bytes(), self.next_err)
        failed = [e for e in self.events if e["stage"] == "command-observation-failed"]
        self.assertEqual(failed[-1]["status"], status)
        self.assertEqual((failed[-1]["stdout"], failed[-1]["stderr"]), (stdout, stderr))
        for name in (stdout, stderr):
            self.assertEqual(Path(name).suffix, ".log", "failed output must survive evidence export")


class FixtureClock:
    """Advance poll deadlines without waiting nine real seconds per failure."""

    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, delay):
        # Let the real child processes finish starting, without charging host
        # scheduling time against the diagnostic's virtual startup deadline.
        time.sleep(0.02)
        self.now += delay


class FixtureProcesses:
    Popen = staticmethod(subprocess.Popen)
    STDOUT = subprocess.STDOUT
    SubprocessError = subprocess.SubprocessError
    TimeoutExpired = subprocess.TimeoutExpired

    @staticmethod
    def run(args, *, timeout, **kwargs):
        # Charge deterministic work against the supplied command/deadline budget,
        # independently of Python startup on a loaded host. Ordinary commands
        # need 0.25 virtual seconds; an insufficient budget cannot complete them.
        hung = os.environ["MODE"] == "hung-tree" and args == ["swaymsg", "-t", "get_tree"]
        work = timeout if hung else 0.25
        ime.time.now += min(work, timeout)
        if work > timeout:
            raise subprocess.TimeoutExpired(args, timeout)
        # Keep a real host safety bound, and exercise real timeout/child cleanup
        # for the deliberately hung command rather than simulating its exit.
        return subprocess.run(args, timeout=timeout if hung else 10, **kwargs)


if __name__ == "__main__":
    ime.time = FixtureClock()
    ime.subprocess = FixtureProcesses()
    sys.exit(ime.main())
