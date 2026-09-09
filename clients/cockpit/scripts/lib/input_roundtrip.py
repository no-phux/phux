"""Owned-process and fixture helpers for input-roundtrip.py (macOS only).

SDK snapshot bodies are NOT trusted diagnostics. Fixture parsing below is only
for a private coordinator whose shell startup files and emitted titles we own.
Raw snapshots, command output, pasteboard data and logs never enter the report.
"""

from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import time

from . import dev_diagnostics as identity

require = identity.require
Failure = identity.EvidenceError
ROOT = Path(__file__).resolve().parents[2]
VIEW = r"@w\d+/phux-cockpit-canvas(?:-\d+)?"
WIDGET = re.compile(
    rf'^    widget (?P<view>{VIEW})#(?P<id>\d+) role=(?P<role>\w+) '
    r'name="(?P<name>[^"\r\n]*)" bounds=\([^\r\n]*?\) '
    r'focused=(?P<focused>true|false) enabled=(?P<enabled>true|false)(?: .*)?$'
)


def run(argv, *, cwd=ROOT, env=None, timeout=20):
    result = subprocess.run([str(a) for a in argv], cwd=cwd, env=env,
                            capture_output=True, timeout=timeout, check=False)
    require(result.returncode == 0, "subprocess refused operation")
    return result.stdout


def wait_for(check, description, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise Failure(f"timeout: {description}")


def terminal_ids(report):
    require(report["unreachable"] == [], "partial coordinator inventory")
    resources = report["resources"]
    require(all(r["kind"] == "terminal" for r in resources), "non-terminal fixture resource")
    ids = [r["id"] for r in resources]
    require(all(re.fullmatch(r"@[1-9]\d*", i) for i in ids), "nonlocal or invalid ResourceId")
    require(len(set(ids)) == len(ids), "duplicate ResourceId")
    require(set(ids) == set(report["terminals"]), "inconsistent resource inventory")
    return set(ids)


def added_terminal(before, after):
    require(before <= after, "transition removed a durable terminal")
    added = after - before
    require(len(added) <= 1, "transition created multiple terminals")
    return next(iter(added), None)


def fixture_widgets(raw):
    """In-memory controlled-fixture parser, never a provenance/sanitizing API."""
    return [match.groupdict() for line in raw.decode().splitlines()
            if (match := WIDGET.fullmatch(line))]


def in_view(widget, view):
    return view is None or widget["view"] == view


def find_widget(widgets, role, title, view=None):
    matches = [w for w in widgets if (w["role"], w["name"]) == (role, title) and in_view(w, view)]
    require(len(matches) <= 1, "ambiguous controlled fixture widget")
    return next(iter(matches), None)


def require_owner(widget, title, view):
    require(widget is not None, "intended ResourceId has no visible fixture textbox")
    require(widget["role"] == "textbox", "target is not a terminal textbox")
    require(widget["name"] == title, "fixture title belongs to another ResourceId")
    require(widget["view"] == view, "fixture textbox belongs to another window/view")
    require(widget["enabled"] == "true", "fixture textbox disabled")


def isolated_environment(work, inherited):
    # Allowlist instead of trying to enumerate PHUX knobs, shell hooks, tracing
    # payload switches, SDK overrides and config layers to remove one by one.
    env = {k: inherited[k] for k in ("PATH", "DEVELOPER_DIR", "SDKROOT") if k in inherited}
    env.update(HOME=str(work / "home"), SHELL="/bin/zsh", ZDOTDIR=str(work / "home"),
               XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"),
               XDG_CACHE_HOME=str(work / "cache"), XDG_DATA_HOME=str(work / "data"),
               XDG_RUNTIME_DIR=str(work / "runtime"), TMPDIR=str(work / "tmp"),
               PHUX_SOCKET=str(work / "p.sock"), PHUX_SESSION="default",
               PHUX_COCKPIT_CONFIG=str(work / "cockpit.config"),
               PHUX_COCKPIT_STATE=str(work / "layout.json"),
               PHUX_LOG=str(work / "client.log"), PHUX_LOG_FORMAT="json",
               LC_ALL="C", TERM="xterm-256color")
    return env


def prepare_environment(work):
    env = isolated_environment(work, os.environ)
    require(len(os.fsencode(env["PHUX_SOCKET"])) < 104, "temporary socket path exceeds macOS limit")
    for name in ("home", "config", "state", "cache", "data", "runtime", "tmp"):
        (work / name).mkdir(mode=0o700)
    (work / "cockpit.config").write_text("font-size = 13\n")
    (work / "config/phux").mkdir(mode=0o700)
    (work / "config/phux/config.toml").write_text('[defaults]\nsession-name-template = "default"\n')
    # No inherited prompt hooks, OSC titles, history, shell integration or rc.
    (work / "home/.zshenv").write_text("unset HISTFILE\nunsetopt GLOBAL_RCS\n")
    (work / "home/.zshrc").write_text("PROMPT='rt%# '\nRPROMPT=''\nbindkey -e\n")
    return env


def checkout_artifact(path, base):
    resolved = Path(path).resolve(strict=True)
    require(resolved.is_relative_to(base.resolve()), "artifact is outside this checkout")
    return identity.artifact(resolved)


def preflight(native, phux, ffi):
    require(Path.cwd().resolve() == ROOT, "run from this checkout's clients/cockpit")
    require(not identity.live_publishers(), "another Cockpit publisher is running")
    require(Path(ffi).name == "libphux_client_ffi.a", "select the FFI archive consumed by the build")
    artifacts = {"native": checkout_artifact(native, ROOT),
                 "phux": checkout_artifact(phux, ROOT.parents[1]),
                 "ffi_input": checkout_artifact(ffi, ROOT.parents[1])}
    sdk = Path(artifacts["native"]["path"]).parents[2]
    pinned = identity.sdk_identity(ROOT)
    require(pinned["pin_status"] == "pinned", "SDK must have a published commit pin")
    revision = run(["git", "-C", sdk, "rev-parse", "HEAD"]).decode().strip()
    require(revision == pinned["declared_commit"], "Native CLI checkout differs from SDK pin")
    run(["git", "-C", sdk, "diff", "--quiet", "HEAD", "--"])
    return artifacts, sdk, pinned


def native_dev_argv(native, ffi):
    return [str(native), "dev", "-Doptimize=Debug", "-Dautomation=true", "-Dphux-enabled=true",
            "-Dphux-client-ffi-profile=ffi-dev",
            f"-Dphux-client-ffi-include-dir={ROOT.parents[1] / 'crates/phux-client-ffi/include'}",
            f"-Dphux-client-ffi-lib-dir={Path(ffi).parent}"]


def descendant(pid, ancestor):
    rows = run(["ps", "-axo", "pid=,ppid="]).decode().splitlines()
    parents = dict(tuple(map(int, row.split())) for row in rows)
    seen = set()
    while pid > 1 and pid not in seen:
        if pid == ancestor:
            return True
        seen.add(pid)
        pid = parents.get(pid, 0)
    return False


def stop_child(child):
    if child is None or child.poll() is not None:
        return
    child.terminate()
    try:
        child.wait(timeout=15)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait(timeout=5)
        raise Failure("owned process required SIGKILL during cleanup")


class Launcher:
    def __init__(self, args, work, evidence):
        self.args, self.work, self.evidence = args, work, evidence
        self.env = prepare_environment(work)
        self.server = None
        self.dev = None
        self.app = None
        self.server_identity = None
        self.artifacts, sdk, pin = preflight(args.native, args.phux, args.ffi_lib)
        self.env.update(NATIVE_SDK_PATH=str(sdk), ZIG_GLOBAL_CACHE_DIR=str(ROOT / ".zig-global-cache"))
        evidence.update(source=identity.source_identity(ROOT), artifacts=self.artifacts,
                        sdk_inputs=pin, linked_build_attestation=False, launches=[])

    def spawn(self, argv):
        # App/server logs stay ephemeral. PHUX_LOG is a client/optional tee;
        # telemetry.rs puts the canonical server log below XDG_STATE_HOME/phux.
        return subprocess.Popen([str(a) for a in argv], cwd=ROOT, env=self.env,
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, start_new_session=True)

    def check_inputs(self):
        for artifact in self.artifacts.values():
            identity.check_artifact_stamp(artifact)

    def start_server(self):
        require(not Path(self.env["PHUX_SOCKET"]).exists(), "isolated socket already exists")
        self.server = self.spawn([self.args.phux, "--socket", self.env["PHUX_SOCKET"], "server"])
        wait_for(self.server_ready, "owned coordinator socket")
        self.server_identity = identity.process_identity(self.server.pid)
        require(self.server_identity["executable"] == str(self.args.phux), "wrong coordinator executable")
        self.socket_identity = identity.file_stamp(Path(self.env["PHUX_SOCKET"]).stat())
        self.evidence["coordinator"] = self.server_identity

    def server_ready(self):
        require(self.server.poll() is None, "owned coordinator exited")
        try:
            raw = run([self.args.phux, "--socket", self.env["PHUX_SOCKET"], "status", "--json"],
                      env=self.env, timeout=2)
        except (Failure, subprocess.TimeoutExpired):
            return False
        require(json.loads(raw)["pid"] == self.server.pid, "socket belongs to another coordinator")
        return True

    def check_server(self):
        self.check_inputs()
        require(self.server.poll() is None, "owned coordinator exited")
        require(identity.process_identity(self.server.pid) == self.server_identity,
                "coordinator process identity changed")
        require(identity.file_stamp(Path(self.env["PHUX_SOCKET"]).stat()) == self.socket_identity,
                "coordinator socket identity changed")

    def start_app(self):
        require(not identity.live_publishers(), "another Cockpit publisher is running")
        self.check_inputs()
        self.dev = self.spawn(native_dev_argv(self.args.native, self.args.ffi_lib))
        self.app = wait_for(self.find_app, "native dev publisher (including build)", self.args.startup_timeout)
        self.binary = identity.artifact(ROOT / "zig-out/bin/phux-cockpit")
        self.check_app()
        self.evidence["launches"].append({"process": self.app, "binary_on_disk": self.binary,
                                         "launcher": "official native dev", "requested_optimize": "Debug"})

    def find_app(self):
        require(self.dev.poll() is None, "native dev exited before acceptance")
        pids = identity.live_publishers()
        require(len(pids) <= 1, "multiple Cockpit publishers")
        if not pids:
            return None
        process = identity.process_identity(pids[0])
        require(descendant(pids[0], self.dev.pid), "publisher was not launched by this harness")
        require(process["executable"] == str(ROOT / "zig-out/bin/phux-cockpit"), "wrong app executable")
        require(process["cwd"] == str(ROOT), "wrong app source-root CWD")
        return process

    def check_app(self):
        self.check_server()
        identity.check_publisher(self.app, identity.process_identity(self.app["pid"]), identity.live_publishers())
        identity.check_artifact_stamp(self.binary)

    def stop_app(self):
        try:
            if self.app is not None and self.app["pid"] in identity.live_publishers():
                actual = identity.process_identity(self.app["pid"])
                require(actual == self.app, "refuse to stop replaced publisher")
                os.kill(self.app["pid"], signal.SIGTERM)
                wait_for(lambda: self.app["pid"] not in identity.live_publishers(), "owned app shutdown")
                self.app = None
        finally:
            stop_child(self.dev)
            self.dev = None

    def close(self):
        try:
            self.stop_app()
        finally:
            try:
                stop_child(self.server)
            finally:
                self.evidence["coordinator_stopped"] = self.server is None or self.server.poll() is not None


# AppKit activation requires a GUI login session, not System Events/AX access.
# All pasteboard items/types are held in this process's memory, restored on EOF.
# Refuse to overwrite a pasteboard changed concurrently by another application.
APPKIT = r'''
import AppKit
import Foundation
let board = NSPasteboard.general
var saved: [[NSPasteboard.PasteboardType: Data]]? = nil
var ownedChange: Int? = nil
func save() -> [[NSPasteboard.PasteboardType: Data]]? {
    var result: [[NSPasteboard.PasteboardType: Data]] = []
    for item in board.pasteboardItems ?? [] {
        var fields: [NSPasteboard.PasteboardType: Data] = [:]
        for type in item.types {
            guard let data = item.data(forType: type) else { return nil }
            fields[type] = data
        }
        result.append(fields)
    }
    return result
}
func item(_ fields: [NSPasteboard.PasteboardType: Data]) -> NSPasteboardItem {
    let result = NSPasteboardItem()
    for (type, data) in fields { result.setData(data, forType: type) }
    return result
}
func unchanged() -> Bool {
    guard let count = ownedChange else { return true }
    return board.changeCount == count
}
func restore() -> Bool {
    guard let original = saved else { return true }
    guard unchanged() else { return false }
    let items = original.map { item($0) }
    board.clearContents()
    if !items.isEmpty {
        guard board.writeObjects(items) else { return false }
    }
    saved = nil
    ownedChange = nil
    return true
}
defer { _ = restore() }
func replace(_ text: String) -> Bool {
    guard unchanged() else { return false }
    if saved == nil {
        let before = board.changeCount
        saved = save()
        if board.changeCount != before { saved = nil }
    }
    guard saved != nil else { return false }
    board.clearContents()
    let ok = board.setString(text, forType: .string)
    ownedChange = board.changeCount
    return ok
}
func activate(_ value: Substring) -> Bool {
    guard let pid = Int32(value) else { return false }
    return NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateAllWindows]) ?? false
}
func respond(_ line: String) -> Bool {
    if line == "restore" {
        return restore()
    }
    if line.hasPrefix("activate ") {
        return activate(line.dropFirst(9))
    }
    guard let data = Data(base64Encoded: line), let text = String(data: data, encoding: .utf8) else { return false }
    return replace(text)
}
while let line = readLine() {
    print(respond(line) ? "ok" : "unavailable")
    fflush(stdout)
}
'''


class AppKit:
    def __init__(self, work, env):
        script = work / "appkit.swift"
        script.write_text(APPKIT)
        # Native/Zig may intentionally use a Nix SDK. /usr/bin/swift must use
        # its selected Xcode SDK, not an inherited, incompatible Swift module.
        swift_env = dict(env)
        swift_env.pop("SDKROOT", None)
        swift_env.pop("DEVELOPER_DIR", None)
        self.child = subprocess.Popen(["/usr/bin/swift", str(script)], env=swift_env,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=subprocess.DEVNULL)

    def request(self, message):
        self.child.stdin.write((message + "\n").encode())
        self.child.stdin.flush()
        ready, _, _ = select.select([self.child.stdout], [], [], 60)
        require(bool(ready), "AppKit helper timed out (Swift/GUI session required)")
        require(self.child.stdout.readline() == b"ok\n", "AppKit operation refused or pasteboard changed")

    def activate(self, pid):
        self.request(f"activate {pid}")

    def paste(self, text):
        self.request(base64.b64encode(text.encode()).decode())

    def close(self):
        try:
            self.request("restore")
        finally:
            self.child.stdin.close()
            self.child.wait(timeout=10)
