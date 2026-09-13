#!/usr/bin/env python3
"""Serial AppKit daily-journey smoke. See everyday-ux-README.md for evidence scope."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from typing import NamedTuple

ROOT = Path(__file__).resolve().parents[1]
CHECKOUT = ROOT.parents[1]
FFI_HEADER = CHECKOUT / "crates/phux-client-ffi/include/phux/client.h"


class InfrastructureError(RuntimeError):
    """The instrument cannot establish a product verdict."""


def run(argv, **kwargs):
    try:
        return subprocess.check_output(argv, text=True, stderr=subprocess.STDOUT,
                                       timeout=30, **kwargs).strip()
    except subprocess.CalledProcessError as error:
        raise InfrastructureError(f"command failed ({error.returncode}): {argv!r}\n{error.output}") from error


def wait_for(probe, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = probe()
        if value:
            return value
        time.sleep(0.15)
    raise InfrastructureError(f"timed out: {probe.__name__}")


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def applescript_string(value):
    return json.dumps(value, ensure_ascii=False)


def ensure_serial(expected=None):
    pids = set()
    for name in ("phux-cockpit", "phux-cockpit-dev"):
        found = subprocess.run(["pgrep", "-x", name], capture_output=True, text=True, check=False)
        pids.update(int(pid) for pid in found.stdout.split())
    allowed = set() if expected is None else {expected}
    if pids != allowed:
        raise InfrastructureError(f"serial live ownership changed: expected {allowed}, found {pids}")


def publisher(snapshot):
    header = snapshot.partition("\n")[0]
    match = re.search(r"\bpublisher_pid=(\d+)\b", header)
    if not match:
        raise InfrastructureError("snapshot has no publisher PID")
    return int(match[1])


class Widget(NamedTuple):
    identity: str
    role: str
    name: str
    parent: str


def active_window(snapshot):
    match = re.search(r"^window @w(\d+).* focused=true\b", snapshot, re.M)
    return match[1] if match else "1"


def visible_widgets(snapshot, window=None):
    pattern = (r'^\s*widget @w(\d+)/[^\s#]+#(\d+) role=(\S+) name="([^"\n]*)" '
               r'bounds=\([-\d.]+,[-\d.]+ ([-\d.]+)x([-\d.]+)\) focused=\S+ enabled=(\S+)([^\n]*)')
    selected = str(window or active_window(snapshot))
    records = re.findall(pattern, snapshot, re.M)
    return [Widget(identity, role, name, parent_id(tail))
            for owner, identity, role, name, width, height, enabled, tail in records
            if owner == selected and enabled == "true" and float(width) > 0 and float(height) > 0]


def parent_id(tail):
    match = re.search(r"\bparent=#(\d+)", tail)
    return match[1] if match else ""


def named(snapshot, text, role=None, window=None):
    roles = (role,) if isinstance(role, str) else role
    for widget in visible_widgets(snapshot, window):
        if widget.name.casefold() == text.casefold() and (roles is None or widget.role in roles):
            return True
    return False


def list_rows(snapshot, label, window=None):
    widgets = visible_widgets(snapshot, window)
    containers = {widget.identity for widget in widgets if widget.role == "list" and widget.name == label}
    return sum(widget.role == "listitem" and widget.parent in containers for widget in widgets)


def stop_child(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=15)


def cancelled(signum, _frame):
    raise InfrastructureError(f"cancelled by signal {signum}")


def isolated_environment(work):
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("PHUX_", "XDG_", "NATIVE_SDK_"))}
    for key in ("ENV", "BASH_ENV", "ZDOTDIR", "SSH_AUTH_SOCK"):
        env.pop(key, None)
    env.update(HOME=str(work / "home"), SHELL="/bin/sh",
               XDG_CONFIG_HOME=str(work / "config"),
               XDG_DATA_HOME=str(work / "data"), XDG_STATE_HOME=str(work / "state"),
               XDG_CACHE_HOME=str(work / "cache"), XDG_RUNTIME_DIR=str(work / "runtime"),
               PHUX_SOCKET=str(work / "phux.sock"), PHUX_SESSION="everyday-ux",
               PHUX_COCKPIT_CONFIG=str(work / "Cockpit config"),
               PHUX_COCKPIT_STATE=str(work / "workspace.state"),
               PHUX_LOG=str(work / "phux.log"), TMPDIR=str(work))
    for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
                "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
        Path(env[key]).mkdir()
    return env


class Journey:
    def __init__(self, work, native, phux):
        self.work, self.native, self.phux = work, native, phux
        self.env = isolated_environment(work)
        self.app = None
        self.server = None
        self.results = []
        self.sequence = 0
        self.handles = []
        self.provenance = {}
        self.ffi_archive = None

    def save(self, name, content):
        (self.work / name).write_text(content + "\n")

    def spawn(self, argv, log):
        handle = (self.work / log).open("w")
        self.handles.append(handle)
        return subprocess.Popen(argv, env=self.env, cwd=self.work,
                                stdout=handle, stderr=subprocess.STDOUT)

    def cli(self, *args):
        return run([self.phux, "--socket", self.env["PHUX_SOCKET"], *args], env=self.env)

    def registry(self, *args):
        # Registry verbs are offline and correctly reject an explicit --socket.
        return run([self.phux, "host", *args], env=self.env)

    def server_ready(self):
        if self.server.poll() is not None:
            raise InfrastructureError("private server exited; see server.log")
        return Path(self.env["PHUX_SOCKET"]).exists()

    def setup(self, bundle):
        ensure_serial()
        self.save("Cockpit config", "# Isolated daily-journey configuration\nfont-size = 13")
        registry = Path(self.env["XDG_CONFIG_HOME"]) / "phux/config.toml"
        registry.parent.mkdir()
        registry.write_text("# Isolated empty machine registry\n")
        editor = self.work / "Fixture editor"
        editor.write_text(f"#!{sys.executable}\nimport json, pathlib, sys\n"
                          f"pathlib.Path({str(self.work / 'editor-argv.json')!r}).write_text(json.dumps(sys.argv[1:]))\n"
                          "print('EVERYDAY_UX_EDITOR_READY', flush=True)\ninput()\n")
        editor.chmod(0o755)
        self.env["VISUAL"] = f'"{editor}" --fixture-argument'
        self.env["EDITOR"] = "/usr/bin/false"
        self.server = self.spawn([self.phux, "server", "--session", "everyday-ux",
                                  "--listen", "127.0.0.1:0", "--quic", "127.0.0.1:0",
                                  "--exit-after-idle", "120"], "server.log")
        wait_for(self.server_ready)
        server = json.loads(self.cli("status", "--json"))
        if server["pid"] != self.server.pid:
            raise InfrastructureError("socket is not owned by the private server")
        staged = self.work / "Phux Cockpit (dev).app"
        executable = run(["bash", "-c", 'source "$1"; dev_app_stage "$2" "$3"',
                          "everyday-ux", str(ROOT / "scripts/lib/dev-app.sh"),
                          str(bundle), str(staged)])
        self.app = self.spawn([executable], "app.log")
        self.provenance.update(app_executable=executable, app_sha256=sha256(executable),
                               publisher_pid=self.app.pid, server=server,
                               config=self.env["PHUX_COCKPIT_CONFIG"],
                               socket=self.env["PHUX_SOCKET"], state=self.env["PHUX_COCKPIT_STATE"],
                               environment=self.env_subset(), native=self.native,
                               native_sha256=sha256(self.native), phux=self.phux,
                               phux_sha256=sha256(self.phux), phux_version=run([self.phux, "--version"]))
        self.provenance["ffi"] = dict(archive=str(self.ffi_archive), archive_sha256=sha256(self.ffi_archive),
                                       header=str(FFI_HEADER), header_sha256=sha256(FFI_HEADER))
        self.save("rust-source.diff", run(["git", "diff", "HEAD", "--", "crates", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml"], cwd=CHECKOUT))
        with (staged / "Contents/Info.plist").open("rb") as handle:
            self.provenance["bundle"] = plistlib.load(handle)
        self.save("provenance.json", json.dumps(self.provenance, indent=2, default=str))
        wait_for(self.snapshot_ready, 30)
        self.snapshot("ready-before-activation")
        self.activate()

    def env_subset(self):
        return {key: value for key, value in self.env.items()
                if key.startswith(("PHUX_", "XDG_")) or key in ("HOME", "VISUAL", "EDITOR")}

    def snapshot_ready(self):
        if self.app.poll() is not None:
            raise InfrastructureError("Cockpit exited; see app.log")
        path = self.work / ".zig-cache/native-sdk-automation/snapshot.txt"
        if not path.exists():
            return False
        text = path.read_text()
        return publisher(text) == self.app.pid and "ready=true" in text

    def snapshot(self, label):
        ensure_serial(self.app.pid)
        if self.app.poll() is not None:
            raise InfrastructureError("Cockpit is no longer running")
        text = run([self.native, "automate", "snapshot"], cwd=self.work)
        if publisher(text) != self.app.pid:
            raise InfrastructureError("automation publisher changed")
        self.sequence += 1
        self.save(f"{self.sequence:02}-{label}.snapshot.txt", text)
        return text

    def applescript(self, body):
        return run(["osascript", "-e", 'tell application "System Events"\n' + body + "\nend tell"])

    def frontmost(self):
        return self.applescript("get unix id of first process whose frontmost is true")

    def activate(self):
        enabled = self.applescript("get UI elements enabled")
        self.save("system-events.json", json.dumps(dict(accessibility_enabled=enabled, frontmost=self.frontmost())))
        if enabled != "true":
            raise InfrastructureError("System Events UI elements enabled=false; grant Accessibility to the hosting automation process in System Settings, then rerun")
        wait_for(self.try_activate)

    def try_activate(self):
        self.applescript(f"set frontmost of (first process whose unix id is {self.app.pid}) to true")
        actual = self.frontmost()
        self.save("activation.json", json.dumps(dict(expected=self.app.pid, actual=actual)))
        return actual == str(self.app.pid)

    def pointer_click(self, point):
        match = re.fullmatch(r"(-?\d+),(-?\d+)", point)
        if not match:
            raise InfrastructureError(f"invalid AX pointer coordinates: {point!r}")
        x, y = match.groups()
        script = (
            'ObjC.import("CoreGraphics");'
            f'const p=$.CGPointMake({x},{y});'
            'function post(type){const event=$.CGEventCreateMouseEvent(null,type,p,$.kCGMouseButtonLeft);'
            '$.CGEventPost($.kCGHIDEventTap,event);}'
            'post($.kCGEventMouseMoved);delay(0.1);post($.kCGEventLeftMouseDown);'
            'delay(0.08);post($.kCGEventLeftMouseUp);'
        )
        run(["osascript", "-l", "JavaScript", "-e", script])

    def pointer_scroll(self, point, lines=-12):
        match = re.fullmatch(r"(-?\d+),(-?\d+)", point)
        if not match:
            raise InfrastructureError(f"invalid AX scroll coordinates: {point!r}")
        x, y = match.groups()
        script = (
            'ObjC.import("CoreGraphics");'
            f'const p=$.CGPointMake({x},{y});'
            'const move=$.CGEventCreateMouseEvent(null,$.kCGEventMouseMoved,p,$.kCGMouseButtonLeft);'
            '$.CGEventPost($.kCGHIDEventTap,move);delay(0.1);'
            f'const wheel=$.CGEventCreateScrollWheelEvent(null,$.kCGScrollEventUnitLine,1,{lines});'
            '$.CGEventPost($.kCGHIDEventTap,wheel);'
        )
        run(["osascript", "-l", "JavaScript", "-e", script])

    def scroll_active_window(self):
        self.snapshot("before-input")
        if self.frontmost() != str(self.app.pid):
            raise InfrastructureError("AppKit focus changed; refusing to scroll another app")
        point = self.applescript(f'tell (first process whose unix id is {self.app.pid})\n'
                                 'set p to position of window 1\nset s to size of window 1\n'
                                 'set targetX to round ((item 1 of p) + (item 1 of s) * 3 / 4)\n'
                                 'set targetY to round ((item 2 of p) + (item 2 of s) * 5 / 8)\n'
                                 'return (targetX as text) & "," & (targetY as text)\nend tell')
        self.pointer_scroll(point)
        time.sleep(0.3)

    def input(self, statement):
        self.snapshot("before-input")
        if self.frontmost() != str(self.app.pid):
            raise InfrastructureError("AppKit focus changed; refusing to type into another app")
        result = self.applescript(statement)
        time.sleep(0.3)
        return result

    def click_named(self, name):
        """Host AX locates the target; CoreGraphics sends a real pointer click."""
        self.snapshot("before-input")
        if self.frontmost() != str(self.app.pid):
            raise InfrastructureError("AppKit focus changed; refusing to click another app")
        point = self.applescript(f'tell (first process whose unix id is {self.app.pid})\n'
                                 'set allElements to entire contents of window 1\n'
                                 'repeat with elementRef in allElements\n'
                                 'try\n'
                                 'set candidate to contents of elementRef\n'
                                 f'if description of candidate is {applescript_string(name)} and role of candidate is "AXButton" and enabled of candidate then\n'
                                 'set p to position of candidate\nset s to size of candidate\n'
                                 'if (item 1 of s) > 0 and (item 2 of s) > 0 then\n'
                                 'set centerX to round ((item 1 of p) + (item 1 of s) / 2)\n'
                                 'set centerY to round ((item 2 of p) + (item 2 of s) / 2)\n'
                                 'set wp to position of window 1\nset ws to size of window 1\n'
                                 'return (centerX as text) & "," & (centerY as text) & "," & '
                                 '(item 1 of wp as text) & "," & (item 2 of wp as text) & "," & '
                                 '(item 1 of ws as text) & "," & (item 2 of ws as text)\n'
                                 'end if\nend if\non error\nend try\nend repeat\nreturn "absent"\nend tell')
        if point == "absent":
            return False
        values = [int(value) for value in point.split(",")]
        if len(values) != 6:
            raise InfrastructureError(f"invalid AX target coordinates: {point!r}")
        center_x, center_y, window_x, window_y, window_width, window_height = values
        if not (window_x <= center_x < window_x + window_width and window_y <= center_y < window_y + window_height):
            return False
        self.pointer_click(f"{center_x},{center_y}")
        time.sleep(0.3)
        return True

    def chord(self, key, modifiers="command down"):
        self.input(f"keystroke {applescript_string(key)} using {{{modifiers}}}")

    def escape(self):
        self.input("key code 53")

    def assertion(self, name, passed, detail):
        result = dict(name=name, passed=bool(passed), detail=detail)
        self.results.append(result)
        self.save("assertions.json", json.dumps(self.results, indent=2))
        print(f"{'PASS' if passed else 'FAIL'} {name}: {detail}", flush=True)

    def menu(self, title):
        self.snapshot("before-menu")
        self.activate()
        text = self.applescript(f'tell (first process whose unix id is {self.app.pid})\n'
                               f'get name of every menu item of menu 1 of menu bar item {applescript_string(title)} of menu bar 1\nend tell')
        self.save(f"menu-{title}.txt", text)
        return text

    def menu_click(self, title, item):
        self.input(f'tell (first process whose unix id is {self.app.pid})\n'
                   f'click menu item {applescript_string(item)} of menu 1 of menu bar item {applescript_string(title)} of menu bar 1\nend tell')

    def commands(self):
        before = self.snapshot("commands-before")
        window = active_window(before)
        self.assertion("Commands initially closed", not named(before, "Search commands", role="textbox"), "negative control")
        self.chord("p", "command down, shift down")
        after = self.snapshot("commands-open")
        self.assertion("Cmd+Shift+P opens Commands", named(after, "Search commands", role="textbox", window=window),
                       "rendered search field must name Commands; a menu label is insufficient")
        self.escape()

    def terminal_input(self):
        before = self.cli("snapshot", "everyday-ux", "--json")
        self.save("terminal-before.json", before)
        self.assertion("Input marker initially absent", "CUX_INPUT_OK" not in before, "independent CLI observer")
        self.input('keystroke "printf \'CUX_%s\\\\n\' \'INPUT_OK\'"')
        self.input("key code 36")
        time.sleep(0.5)
        after = self.cli("snapshot", "everyday-ux", "--json")
        self.save("terminal-after.json", after)
        self.assertion("AppKit shell input reaches dedicated session", "CUX_INPUT_OK" in after,
                       "marker observed by independent CLI, not speculative local echo")

    def machines(self):
        before = self.snapshot("machines-before")
        window = active_window(before)
        self.assertion("Machines discoverable in terminal chrome", named(before, "Machines", role="button"),
                       "visible rendered Machines action before typing any hostname")
        if named(before, "Machines", role="button"):
            self.assertion("Machines pointer activation", self.click_named("Machines"), "host accessibility target and actual pointer click")
        else:
            self.chord("o", "command down, shift down")
        after = self.snapshot("machines-open")
        self.assertion("Empty saved-machine journey", named(after, "No remote machines added", role="text", window=window) and named(after, "Add Machine", role="button", window=window),
                       "private empty registry: empty explanation and Add Machine action")
        self.escape()

    def saved_machines(self):
        """Registry writes use the CLI; SSH destinations are loopback and never activated."""
        labels = [f"Fixture machine {index}" for index in range(1, 7)]
        for index, label in enumerate(labels, 1):
            self.registry("add", label, f"ssh://fixture{index}@127.0.0.1", "--json")
        self.registry("add", "Fixture satellite", "ssh://satellite@127.0.0.1", "--role", "satellite", "--disabled", "--json")
        self.save("registry-populated.json", self.registry("ls", "--json"))
        before = self.snapshot("saved-machines-before")
        window = active_window(before)
        if named(before, "Machines", role="button"):
            self.click_named("Machines")
        else:
            self.chord("o", "command down, shift down")
        after = self.snapshot("saved-machines-open")
        for label in labels + ["Fixture satellite"]:
            self.assertion(f"Saved inventory discovers {label}", named(after, label, role=("listitem", "button"), window=window),
                           "actual CLI registry entry; no connection or enrollment attempted")
        self.escape()

    def windows(self):
        self.chord("n")
        after = self.snapshot("second-window")
        self.assertion("New Window creates a second platform window", len(re.findall(r"^window @", after, re.M)) == 2,
                       "runtime platform windows, not a fixture window list")
        menu = self.menu("Window")
        available = "Show All Windows…" in menu
        self.assertion("Window menu offers Show All Windows", available, "actual AppKit menu inventory")
        if available:
            window = active_window(after)
            self.assertion("Window overview initially closed", not named(after, "Open windows", role="list"), "negative control")
            self.menu_click("Window", "Show All Windows…")
            overview = self.snapshot("windows-overview")
            rows = list_rows(overview, "Terminals and sessions", window)
            self.assertion("Window overview opens", named(overview, "Terminals and sessions", role="list", window=window) and rows >= 2,
                            "window/session list contains both created windows after actual menu activation")
            self.escape()

    def settings(self):
        window = active_window(self.snapshot("settings-before"))
        self.chord(",")
        after = self.snapshot("settings-open")
        for group in ("Appearance", "Terminal", "Keyboard", "Window"):
            self.assertion(f"Settings group {group}", named(after, group, role="tab", window=window), "rendered settings catalog")
        self.assertion("Settings searchable", named(after, "Search settings", role="textbox", window=window), "accessible search field")
        offered = self.find_editor(after, window)
        self.assertion("Edit Configuration offered", offered,
                       "editing must be distinct from Reveal Configuration in Finder")
        if offered:
            self.editor()
        self.escape()

    def find_editor(self, snapshot, window):
        if named(snapshot, "Edit Configuration", role="button", window=window):
            return True
        for section in ("Terminal", "Keyboard", "Window", "Workspace", "Connection"):
            if named(snapshot, section, role="tab", window=window):
                if not self.click_named(section):
                    raise InfrastructureError(f"rendered Settings section {section!r} has no host accessibility target")
                current = self.snapshot("settings-" + section.lower())
                if named(current, "Edit Configuration", role="button", window=window):
                    return True
        return False

    def editor(self):
        marker = self.work / "editor-argv.json"
        self.assertion("Editor not already launched", not marker.exists(), "negative control")
        clicked = self.click_named("Edit Configuration")
        for _ in range(6):
            if clicked:
                break
            self.scroll_active_window()
            clicked = self.click_named("Edit Configuration")
        self.assertion("Edit Configuration pointer activation", clicked, "actual AppKit pointer input after scrolling when needed")
        deadline = time.monotonic() + 10
        while not marker.exists() and time.monotonic() < deadline:
            time.sleep(0.2)
        argv = json.loads(marker.read_text()) if marker.exists() else []
        self.assertion("Editor opens actual local config with arguments", argv == ["--fixture-argument", self.env["PHUX_COCKPIT_CONFIG"]],
                       f"fixture process observed argv={argv!r}; VISUAL has a quoted executable path with spaces")
        self.save("editor-server.json", self.cli("ls", "--json"))
        self.snapshot("editor-result")

    def finish(self):
        self.save("server-after.json", self.cli("status", "--json"))
        self.save("results.json", json.dumps(dict(assertions=self.results,
                  failed=sum(not item["passed"] for item in self.results),
                  evidence_scope="Real AppKit input plus runtime accessibility and server observations; no pixel fidelity claim"), indent=2))
        return int(any(not item["passed"] for item in self.results))

    def close(self):
        errors = []
        for process in (self.app, self.server):
            try:
                stop_child(process)
            except (OSError, subprocess.SubprocessError) as error:
                errors.append(str(error))
        for handle in self.handles:
            try:
                handle.close()
            except OSError as error:
                errors.append(str(error))
        if errors:
            self.save("cleanup-error.txt", "\n".join(errors))
            raise InfrastructureError("child cleanup failed; see cleanup-error.txt")


def build_environment():
    env = {key: value for key, value in os.environ.items() if not key.startswith("PHUX_CLIENT_FFI_")}
    env.pop("CARGO_BUILD_TARGET", None)
    env.update(CARGO_TARGET_DIR=str(CHECKOUT / "target"),
               CARGO_BUILD_JOBS=os.environ.get("CARGO_BUILD_JOBS", "2"))
    return env


def native_target():
    version = run(["rustc", "-vV"], cwd=CHECKOUT, env=build_environment())
    match = re.search(r"^host: ([a-zA-Z0-9_-]+)$", version, re.M)
    if not match:
        raise InfrastructureError("rustc -vV did not report a usable host target")
    return match[1]


def ffi_archive(target):
    return CHECKOUT / "target" / target / "ffi-dev/libphux_client_ffi.a"


def build(work, target):
    # Explicit --target outranks Cargo's project/global build.target configuration.
    # Even a same-host --target adds the triple directory to Cargo's output path.
    commands = (["cargo", "rustc", "--locked", "--manifest-path", str(CHECKOUT / "Cargo.toml"),
                 "--profile", "ffi-dev", "--target", target, "-p", "phux-client-ffi",
                 "--lib", "--crate-type", "staticlib"],
                [str(ROOT / "scripts/zig-build.sh"), "package", "-j2", "-Dautomation=true",
                 "-Dphux-enabled=true", "-Dphux-client-ffi-profile=ffi-dev", "-Doptimize=ReleaseSafe",
                 f"-Dphux-client-ffi-include-dir={FFI_HEADER.parents[1]}",
                 f"-Dphux-client-ffi-lib-dir={ffi_archive(target).parent}"])
    env = build_environment()
    for index, command in enumerate(commands):
        with (work / f"build-{index}.log").open("w") as log:
            subprocess.run(command, cwd=(CHECKOUT if index == 0 else ROOT), env=env,
                           stdout=log, stderr=subprocess.STDOUT, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", type=Path, required=True, help="CLI from scripts/build-automation-cli.sh")
    parser.add_argument("--phux", type=Path, default=Path(shutil.which("phux") or "phux"))
    parser.add_argument("--artifacts", type=Path, help="new artifact directory (must not exist)")
    parser.add_argument("--no-build", action="store_true", help="existing package; source/build binding is UNVERIFIED")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, cancelled)
    ensure_serial()
    work = args.artifacts or Path(tempfile.mkdtemp(prefix="cux-", dir="/private/tmp/opencode"))
    if args.artifacts:
        work.mkdir(parents=True)
    print(f"Artifacts: {work}", flush=True)
    journey = Journey(work.resolve(), str(args.native.resolve()), str(args.phux.resolve()))
    journey.provenance.update(source_root=str(ROOT), source_commit=run(["git", "rev-parse", "HEAD"], cwd=ROOT),
                              cockpit_source_tree=run(["git", "rev-parse", "HEAD:clients/cockpit/src"], cwd=ROOT),
                              harness_sha256=sha256(__file__),
                              source_diff=run(["git", "diff", "HEAD", "--", "src", "app.zon", "build.zig", "build.zig.zon"], cwd=ROOT),
                              build_binding="unverified existing package" if args.no_build else "built by this run")
    try:
        target = native_target()
        journey.ffi_archive = ffi_archive(target)
        journey.provenance["cargo_target"] = target
        if not args.no_build:
            build(work, target)
        journey.setup(ROOT / "zig-out/package/phux-cockpit.app")
        journey.terminal_input()
        journey.commands()
        journey.machines()
        journey.saved_machines()
        journey.windows()
        journey.settings()
        return journey.finish()
    except (InfrastructureError, subprocess.SubprocessError) as error:
        journey.save("infrastructure-error.txt", str(error))
        print(f"INFRASTRUCTURE ERROR: {error}", file=sys.stderr)
        return 2
    finally:
        journey.close()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (InfrastructureError, OSError) as error:
        print(f"INFRASTRUCTURE ERROR: {error}", file=sys.stderr)
        sys.exit(2)
