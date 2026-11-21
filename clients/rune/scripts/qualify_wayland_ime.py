"""Private-bus IME diagnostic for qualify-wayland.sh (never run on a real display).

One shared 9s startup budget replaces 3s + 6s sleeps; no quiet-frame gate,
composition retry, or Unicode injection. Raw WAYLAND_DEBUG logs survive success
and failure. Tracing perturbs timing and does not establish a root-cause repair.
"""

from datetime import datetime, timezone
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

STARTUP_SECONDS = 9
ECHO_SECONDS = 2
POLL_SECONDS = 0.1
KEYS = ["wtype", "-k", "Control_L", "-s", "500", "nihao",
        "-s", "800", "1", "-s", "500", "-k", "Return"]
ECHO = r"^│\$ 你好"
DBUS = ["dbus-send", "--session", "--print-reply"]
CONTROLLER = [*DBUS, "--dest=org.fcitx.Fcitx5", "/controller",
              "org.fcitx.Fcitx.Controller1.DebugInfo"]
BUS = [*DBUS, "--dest=org.freedesktop.DBus", "/org/freedesktop/DBus"]


def rune_focused(node, pid):
    """Sway's focused leaf is the keyboard owner, not an ancestor focus list."""
    if (node.get("pid"), node.get("app_id"), node.get("focused")) == (
            pid, "sh.phux.rune", True):
        return True
    children = node.get("nodes", []) + node.get("floating_nodes", [])
    return any(rune_focused(child, pid) for child in children)


def focused_context(info):
    # DebugInfo is human-readable, not a stable API. Accept only the known
    # program/frontend/focus tokens on ONE context line; retain raw output if
    # the installed fcitx changes it. Do not infer focus from the profile.
    for line in info.replace(r"\n", "\n").splitlines():
        tokens = dict(re.findall(r"\b(program|frontend|focus):([^\s\"\\]+)", line))
        if (tokens.get("program"), tokens.get("frontend"), tokens.get("focus")) == (
                "sh.phux.rune", "wayland_v2", "1"):
            return True
    return False


def keyboard_capability(capabilities):
    if type(capabilities) is not int or capabilities < 0:
        raise ValueError("seat0 capabilities must be a nonnegative integer")
    return bool(capabilities & 2)


def seat_keyboard_ready(reply):
    seats = json.loads(reply)
    if not isinstance(seats, list):
        raise ValueError("GET_SEATS must return a seat list")
    ready = False
    for seat in seats:
        if not isinstance(seat, dict):
            raise ValueError("GET_SEATS entries must be seat objects")
        if seat.get("name") == "seat0":
            ready = keyboard_capability(seat.get("capabilities"))
    return ready


class Diagnostic:
    def __init__(self, scratch, binary, phux, socket, session):
        self.root = Path(scratch)
        self.snapshot = [phux, "--socket", socket, "snapshot", session]
        self.app_args = [binary, "--socket", socket, "--session", session]
        self.children = {}
        self.started = time.monotonic()
        self.deadline = self.started + STARTUP_SECONDS

    def event(self, stage, **fields):
        record = {"time": datetime.now(timezone.utc).isoformat(),
                  "elapsed": time.monotonic() - self.started, "stage": stage, **fields}
        # qualify.sh exports *.log/*.json/*.png, not *.jsonl or *.stderr.
        with (self.root / "wayland-ime-stages.log").open("a") as log:
            log.write(json.dumps(record, ensure_ascii=False) + "\n")

    def remaining_timeout(self, artifact, timeout):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            self.event("command-not-executed", artifact=artifact, status="deadline-expired")
            raise RuntimeError("deadline expired before " + artifact)
        return min(timeout, remaining)

    def capture_command(self, args, artifact, timeout, stdout, stderr):
        self.remaining_timeout(artifact, timeout)
        with stdout.open("wb") as out, stderr.open("wb") as err:
            timeout = self.remaining_timeout(artifact, timeout)
            result = subprocess.run(args, stdout=out, stderr=err,
                                    timeout=timeout, check=False)
        self.event("command-exit", argv=args, returncode=result.returncode, timeout=timeout)
        if result.returncode:
            raise RuntimeError(f"command failed ({result.returncode}): {args}")

    def failed_attempt(self, artifact, stdout, stderr, error):
        outputs = {}
        for stream, source in (("stdout", stdout), ("stderr", stderr)):
            destination = artifact + f".failed.{stream}.log"
            if not source.exists():
                source.touch()
            source.replace(self.root / destination)
            outputs[stream] = destination
        self.event("command-observation-failed", artifact=artifact,
                   status=type(error).__name__, error=str(error), **outputs)

    def command(self, args, artifact, timeout=1):
        # Canonical artifacts are the last completed successful observation,
        # never in-progress output. Even an unexecuted probe must not erase one.
        self.remaining_timeout(artifact, timeout)
        self.event("command-start", argv=args, artifact=artifact, timeout_cap=timeout)
        with tempfile.TemporaryDirectory(prefix="wayland-ime-command-", dir=self.root) as directory:
            stdout, stderr = Path(directory) / "stdout", Path(directory) / "stderr"
            try:
                self.capture_command(args, artifact, timeout, stdout, stderr)
            except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                self.failed_attempt(artifact, stdout, stderr, error)
                raise
            # A zero exit publishes raw bytes even if a caller's parser rejects
            # them. Each rename is atomic and stays on the scratch filesystem.
            stdout.replace(self.root / artifact)
            stderr.replace(self.root / (artifact + ".stderr.log"))
            self.event("command-observation-published", artifact=artifact)
        return (self.root / artifact).read_text(errors="replace")

    def spawn(self, name, args, logfile):
        # Foreground fcitx (no -d): this PID, like Rune's, is owned and reaped.
        with (self.root / logfile).open("w") as log:
            child = subprocess.Popen(["env", "WAYLAND_DEBUG=1", *args],
                                     stdout=log, stderr=subprocess.STDOUT)
        self.children[name] = child
        self.event("process-start", name=name, pid=child.pid, argv=args)

    def alive(self):
        for name, child in self.children.items():
            status = child.poll()
            if status is not None:
                self.event("process-exited", name=name, pid=child.pid, returncode=status)
                raise RuntimeError(f"{name} exited ({status})")

    def wait_for(self, name, predicate):
        self.event("predicate-start", predicate=name)
        while time.monotonic() < self.deadline:
            try:
                self.alive()
                ready = predicate()
                self.alive()
            except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
                raise RuntimeError(f"{name}: {error}") from error
            if time.monotonic() >= self.deadline:
                break
            if ready:
                self.event("predicate-ready", predicate=name)
                return
            time.sleep(min(POLL_SECONDS, max(0, self.deadline - time.monotonic())))
        raise RuntimeError("timed out waiting for " + name)

    def bus_ready(self):
        info = self.command(
            [*BUS, "org.freedesktop.DBus.NameHasOwner", "string:org.fcitx.Fcitx5"],
            "wayland-ime-bus.log")
        return re.search(r"\bboolean true\b", info) is not None

    def seat_ready(self):
        reply = self.command(["swaymsg", "-r", "-t", "get_seats"],
                             "wayland-ime-seats-before.json")
        return seat_keyboard_ready(reply)

    def focused(self):
        tree = self.command(["swaymsg", "-t", "get_tree"], "wayland-ime-tree-before.json")
        return rune_focused(json.loads(tree), self.children["app"].pid)

    def context_ready(self):
        return focused_context(self.command(CONTROLLER, "wayland-ime-context-before.log"))

    def engine_ready(self):
        engine = self.command(["fcitx5-remote", "-n"], "wayland-ime-engine-before.log")
        state = self.command(["fcitx5-remote"], "wayland-ime-state-before.log")
        return (engine.strip(), state.strip()) == ("pinyin", "2")

    def prompt_ready(self):
        text = self.command(self.snapshot, "wayland-ime-terminal-before.log")
        if re.search(ECHO, text, re.MULTILINE):
            raise RuntimeError("composed echo already present before injection (stale session)")
        return re.search(r"^│\$ ", text, re.MULTILINE) is not None

    def echoed(self):
        text = self.command(self.snapshot, "wayland-ime-terminal.log")
        return re.search(ECHO, text, re.MULTILINE) is not None

    def startup(self):
        self.spawn("fcitx", ["fcitx5"], "wayland-fcitx5.log")
        self.wait_for("fcitx owns org.fcitx.Fcitx5", self.bus_ready)
        self.command([*BUS, "org.freedesktop.DBus.GetNameOwner", "string:org.fcitx.Fcitx5"],
                     "wayland-ime-bus-owner.log")
        owner = self.command(
            [*BUS, "org.freedesktop.DBus.GetConnectionUnixProcessID", "string:org.fcitx.Fcitx5"],
            "wayland-ime-bus-pid.log")
        if re.search(r"\buint32 " + str(self.children["fcitx"].pid) + r"\b", owner) is None:
            raise RuntimeError("fcitx D-Bus owner PID does not match the owned daemon")
        # D-Bus ownership can precede fcitx's virtual keyboard. On Sway 1.9,
        # creating text-input only after mapping can miss the focus replay.
        self.wait_for("seat0 keyboard capability before app spawn", self.seat_ready)
        self.event("seat-ready", seat="seat0", artifact="wayland-ime-seats-before.json")
        self.spawn("app", self.app_args, "wayland-ime-app.log")
        self.wait_for("restarted Rune PID has keyboard focus", self.focused)
        self.wait_for("focused Rune Wayland context (DebugInfo)", self.context_ready)
        self.wait_for("fresh IME session prompt", self.prompt_ready)
        self.command(["fcitx5-remote", "-o"], "wayland-ime-activate.log")
        self.wait_for("effective Pinyin engine and active state 2", self.engine_ready)
        self.wait_for("focused Rune Wayland context after activation", self.context_ready)
        self.wait_for("restarted Rune keyboard owner before injection", self.focused)

    def compose(self):
        self.alive()
        if time.monotonic() >= self.deadline:
            raise RuntimeError("shared 9s startup deadline expired before injection")
        self.event("injection-start", pids={n: p.pid for n, p in self.children.items()})
        # One virtual keyboard/keymap, including candidate choice. Control,
        # not Shift (which switches Pinyin to English). Never replay on failure.
        self.deadline = time.monotonic() + 5
        self.command(KEYS, "wayland-ime-wtype.log", timeout=5)
        self.alive()
        self.event("injection-end")
        self.deadline = time.monotonic() + ECHO_SECONDS
        self.wait_for("exact server echo ^│\\$ 你好", self.echoed)
        protocol = (self.root / "wayland-ime-app.log").read_text(errors="replace")
        # No assumption about Pinyin preedit spelling/segmentation. Keep all
        # preedit, serial, cursor, enable/enter, commit/done traffic in raw logs.
        if re.search(r'zwp_text_input_v3@\d+\.commit_string\("你好"\)', protocol) is None:
            raise RuntimeError("server echo exists but app trace has no commit_string(你好)")
        if time.monotonic() >= self.deadline:
            raise RuntimeError("shared 2s echo observation deadline expired")
        self.event("echo-and-app-commit-observed")

    def evidence(self):
        self.event("failure-liveness",
                   processes={n: {"pid": p.pid, "returncode": p.poll()}
                              for n, p in self.children.items()})
        captures = [
            (["swaymsg", "-r", "-t", "get_seats"], "wayland-ime-seats-failure.json"),
            (["swaymsg", "-t", "get_tree"], "wayland-ime-tree-failure.json"),
            (CONTROLLER, "wayland-ime-context-failure.log"),
            (["fcitx5-remote", "-n"], "wayland-ime-engine-failure.log"),
            (["fcitx5-remote"], "wayland-ime-state-failure.log"),
            (self.snapshot, "wayland-ime-terminal-failure.log"),
            (["grim", str(self.root / "wayland-ime-failed.png")], "wayland-ime-grim.log"),
        ]
        for args, artifact in captures:
            # Seven failure-only captures, each capped at 1s; never extends startup.
            self.deadline = time.monotonic() + 1
            try:
                self.command(args, artifact)
            except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                self.event("evidence-error", artifact=artifact, error=str(error))

    def cleanup(self):
        for name, child in reversed(list(self.children.items())):
            self.event("cleanup", name=name, pid=child.pid, returncode=child.poll())
            if child.poll() is None:
                child.terminate()
            try:
                child.wait(timeout=1)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
            self.event("process-reaped", name=name, pid=child.pid, returncode=child.returncode)

    def run(self):
        try:
            self.startup()
            self.compose()
            return 0
        except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
            self.event("failure", error=str(error))
            print("qualify: wayland IME: " + str(error), file=sys.stderr)
            self.evidence()
            return 1
        finally:
            self.cleanup()


def main():
    return Diagnostic(*sys.argv[1:]).run()


if __name__ == "__main__":
    sys.exit(main())
