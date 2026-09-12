#!/usr/bin/env python3
r"""Hermetic macOS Native dev input acceptance, run serially from clients/cockpit.

Prepare the same-checkout Phux CLI, ffi-dev archive and pinned Native CLI first:
  python3 scripts/input-roundtrip.py --native "$NATIVE" \
    --phux ../../target/debug/phux --ffi-lib ../../target/ffi-dev/libphux_client_ffi.a

This command launches its OWN coordinator and official `native dev` Debug loop
(which may build), creates/splits/switches terminals, tests a secondary window,
pastes text and text+newline, then restarts the app against the same coordinator.
It stops its processes and deletes private HOME/config/state/logs after confirmed
exit. An unconfirmed cleanup retains the private directory for recovery.
Only results, counts and identities enter .dev-run/input-roundtrip/*.json.
The SDK still manages its own source-root automation cache; do not export it.

Requires a logged-in macOS GUI session and /usr/bin/swift for AppKit activation
and pasteboard access. The pasteboard is temporarily replaced and restored with
all item types held in memory. Concurrent clipboard changes cause refusal.
No Accessibility/screenshot permission is needed. SDK widget-key focuses its
view internally: PASS proves shipping callback delivery, NOT unassisted OS-key
focus or host rasterization. Snapshot bodies are parsed ONLY as controlled test
fixtures, never as safe general diagnostics or linked-build provenance.
"""

import argparse
import json
from pathlib import Path
import secrets
import shutil
import signal
import tempfile
import time

from lib import dev_diagnostics as identity
from lib.agent_attention import AgentAcceptance
from lib.input_roundtrip import (
    ROOT, AppKit, Failure, Launcher, added_terminal, find_widget, fixture_widgets,
    child_stopped, persisted_state_effect, require, require_owner, run, terminal_ids, wait_for,
)


class Probe:
    def __init__(self, launcher, appkit, evidence):
        self.launcher, self.appkit, self.evidence = launcher, appkit, evidence
        self.titles = {}
        self.groups = []
        self.window_groups = []
        self.nonce = secrets.token_hex(4)

    def server(self, *args):
        self.launcher.check_server()
        raw = run([self.launcher.args.phux, "--socket", self.launcher.env["PHUX_SOCKET"],
                   *args, "--json"], env=self.launcher.env)
        report = json.loads(raw)
        self.launcher.check_server()
        return report

    def inventory(self):
        return terminal_ids(self.server("ls"))

    def screen(self, target):
        return self.server("snapshot", target)

    def fixture(self, target):
        require(target in self.inventory(), "fixture target missing from coordinator")
        title = f"rt-{self.nonce}-r{target[1:]}"
        self.launcher.check_server()
        run([self.launcher.args.phux, "--socket", self.launcher.env["PHUX_SOCKET"],
             "send-keys", target, "C-u", f"printf '\\033]2;{title}\\007'", "Enter"],
            env=self.launcher.env)
        wait_for(lambda: self.screen(target).get("title") == title, "controlled OSC title")
        self.titles[target] = title

    def snapshot(self):
        self.launcher.check_app()
        identity.publication_identity(ROOT, self.launcher.app)
        raw = run([self.launcher.args.native, "automate", "snapshot"], env=self.launcher.env)
        header = identity.sanitize_snapshot(raw, self.launcher.app["pid"])["header"]
        require(header["markup_watch"] == "armed", "Debug markup watcher is not armed")
        require(header["dispatch_errors"] == "0", "SDK dispatch errors observed")
        self.launcher.check_app()
        return fixture_widgets(raw), header

    def await_publication(self):
        def published():
            path = ROOT / ".zig-cache/native-sdk-automation/snapshot.txt"
            if not path.exists():
                return False
            return path.stat().st_mtime > self.launcher.app["started_unix"] + 1
        wait_for(published, "fresh publisher snapshot")
        self.snapshot()

    def automate(self, *args):
        self.snapshot()
        run([self.launcher.args.native, "automate", *args], env=self.launcher.env)
        self.snapshot()

    def widget(self, role, title, view=None):
        return find_widget(self.snapshot()[0], role, title, view)

    def textbox(self, target):
        require(self.screen(target).get("title") == self.titles[target], "coordinator fixture title changed")
        return wait_for(lambda: self.widget("textbox", self.titles[target]), "intended fixture textbox")

    def owner(self, target, view):
        widget = self.textbox(target)
        require_owner(widget, self.titles[target], view)
        return widget

    def key(self, target, view, key, text=None):
        self.owner(target, view)
        args = ["widget-key", view, key]
        if text is not None:
            args.append(text)
        self.automate(*args)

    def click(self, widget):
        require(widget is not None, "fixture control unavailable")
        require(widget["enabled"] == "true", "fixture control disabled")
        self.automate("widget-click", widget["view"], widget["id"])

    def present(self, target, marker):
        return any(line.strip() == marker for line in self.screen(target)["lines"])

    def absent(self, targets, marker, reason="computed result reached an unintended PTY"):
        require(not any(self.present(t, marker) for t in targets), reason)

    def input_text(self, target, view, text, mode):
        if mode == "key":
            self.key(target, view, "p", text)
            return
        suffix = "\n" if mode == "paste-newline" else ""
        self.appkit.paste(text + suffix)
        self.key(target, view, "cmd+v")

    def roundtrip(self, phase, target, view, mode="key"):
        self.evidence["phase"] = phase
        widget = self.owner(target, view)
        require(widget["focused"] == "true", "intended textbox was not focused before widget-key")
        targets = self.inventory()
        # A whole-row arithmetic result is absent from the command's input bytes;
        # an echoed command cannot satisfy the positive observation.
        marker = str(secrets.randbelow(800000000) + 100000000)
        command = f"printf '%s\\n' $(({int(marker) - 451}+451))"
        require(marker not in command, "invalid computed-output fixture")
        self.absent(targets, marker, "computed result fixture already present before input")
        self.key(target, view, "ctrl+u")
        self.input_text(target, view, command, mode)
        wait_for(lambda: any(command in line for line in self.screen(target)["lines"]), "command echo in intended PTY")
        # Both text-only and bracketed text+newline paste must wait for Enter.
        self.absent(targets, marker, "computed result appeared before Enter")
        require(self.inventory() == targets, "typing/paste changed terminal inventory")
        self.key(target, view, "enter")
        wait_for(lambda: self.present(target, marker), "computed result after Enter")
        self.absent(targets - {target}, marker)
        require(self.inventory() == targets, "Enter changed terminal inventory")
        self.owner(target, view)
        header = self.snapshot()[1]
        self.evidence["results"].append({"phase": phase, "status": "PASS", "target": target,
            "view": view, "widget_id": int(widget["id"]), "mode": mode,
            "publisher_pid": self.launcher.app["pid"], "other_ptys_checked": len(targets) - 1,
            "inventory_count": len(targets), "snapshot_header": header})

    def transition(self, phase, action):
        self.evidence["phase"] = phase
        before = self.inventory()
        action()
        target = wait_for(lambda: added_terminal(before, self.inventory()), "one new ResourceId")
        self.fixture(target)
        widget = self.textbox(target)
        self.roundtrip(phase, target, widget["view"])
        return target, widget["view"]

    def start(self):
        self.evidence["phase"] = "coordinator"
        self.launcher.start_server()
        status = self.server("status")
        require(status["pid"] == self.launcher.server.pid, "socket is not served by owned coordinator")
        if not self.inventory():
            self.server("new", "-s", "default")
        targets = self.inventory()
        require(len(targets) == 1, "initial fixture must have exactly one terminal")
        initial = next(iter(targets))
        self.fixture(initial)
        self.evidence["phase"] = "initial-attach"
        self.launcher.start_app()
        self.appkit.activate(self.launcher.app["pid"])
        self.await_publication()
        view = self.textbox(initial)["view"]
        self.groups.append([initial])
        self.window_groups.append([initial])
        self.roundtrip("initial-attach", initial, view)
        return initial, view

    def select_group_target(self, target, group):
        tabs = [self.widget("tab", self.titles[t]) for t in group]
        tabs = [tab for tab in tabs if tab is not None]
        require(len(tabs) == 1, "restored fixture tab ownership ambiguous or missing")
        self.click(tabs[0])
        widget = self.textbox(target)
        require(widget["view"] == tabs[0]["view"], "restored textbox left its tab window/view")
        self.click(widget)
        wait_for(lambda: self.textbox(target)["focused"] == "true", "clicked terminal focus")
        return widget["view"]

    def await_persisted_state(self):
        self.launcher.check_app()
        path = Path(self.launcher.env["PHUX_COCKPIT_STATE"])
        wait_for(lambda: persisted_state_effect(path), "debounced state file before shutdown")
        self.launcher.check_app()
        self.evidence["state_effect_before_shutdown"] = True

    def restored_roundtrips(self):
        windows = {}
        for group in self.groups:
            for target in group:
                view = self.select_group_target(target, group)
                self.require_restored_window(target, view, windows)
                self.roundtrip("restart-attach", target, view)
        self.evidence["restart_window_groups"] = [
            {"targets": group, "window": windows[i]} for i, group in enumerate(self.window_groups)
        ]

    def require_restored_window(self, target, view, windows):
        expected = next(i for i, group in enumerate(self.window_groups) if target in group)
        window = view.split("/", 1)[0]
        if expected in windows:
            require(windows[expected] == window, "restored window group split across windows")
            return
        require(window not in windows.values(), "distinct restored window groups collapsed into one window")
        windows[expected] = window

    def restart(self):
        self.evidence["phase"] = "restart"
        before = self.inventory()
        old = self.launcher.app
        self.await_persisted_state()
        self.launcher.stop_app()
        require(self.inventory() == before, "app exit destroyed durable terminals")
        for target in before:
            require(self.screen(target).get("title") == self.titles[target], "durable title changed after app exit")
        self.launcher.start_app()
        require(self.launcher.app != old, "restart did not launch a fresh process")
        self.appkit.activate(self.launcher.app["pid"])
        self.await_publication()
        require(self.inventory() == before, "restart changed durable terminal identities")
        self.restored_roundtrips()

    def matrix(self):
        initial, main = self.start()
        created, _ = self.transition("toolbar-create", lambda: self.click(self.widget("button", "New terminal", main)))
        split, _ = self.transition("split-right", lambda: self.automate("native-command", "pane.split-right", main))
        self.groups.append([created, split])
        self.window_groups[0].extend([created, split])
        self.evidence["phase"] = "switch-previous"
        self.automate("native-command", "tab.previous", main)
        self.roundtrip("switch-previous", initial, main)
        self.evidence["phase"] = "switch-next"
        self.automate("native-command", "tab.next", main)
        self.roundtrip("switch-next", split, main)
        secondary, second = self.transition("secondary-window", lambda: self.automate("native-command", "window.new", main))
        require(second.split("/", 1)[0] != main.split("/", 1)[0], "new window reused main window")
        self.groups.append([secondary])
        self.window_groups.append([secondary])
        self.roundtrip("paste-text", secondary, second, "paste")
        self.roundtrip("paste-text-newline", secondary, second, "paste-newline")
        AgentAcceptance(self, created, split, main).run()
        self.restart()
        self.evidence["source_at_finish"] = identity.source_identity(ROOT)


def arguments():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--native", type=lambda p: Path(p).resolve(), required=True)
    parser.add_argument("--phux", type=lambda p: Path(p).resolve(), required=True)
    parser.add_argument("--ffi-lib", type=lambda p: Path(p).resolve(), required=True)
    parser.add_argument("--startup-timeout", type=int, default=600, help="native dev build/start budget, seconds")
    return parser.parse_args()


def execute_matrix(args, work, evidence):
    launcher = Launcher(args, work, evidence)
    appkit = None
    evidence["processes_stopped"] = False
    try:
        appkit = AppKit(work, launcher.env)
        Probe(launcher, appkit, evidence).matrix()
    finally:
        close_matrix(launcher, appkit, evidence)


def close_matrix(launcher, appkit, evidence):
    try:
        try:
            if appkit is not None:
                appkit.close()
        finally:
            launcher.close()
    finally:
        helper = None if appkit is None else appkit.child
        evidence["processes_stopped"] = launcher.stopped() and child_stopped(helper)


def private_matrix(args, scratch, evidence):
    work = Path(tempfile.mkdtemp(prefix="rt-", dir=scratch if scratch.is_dir() else None))
    # Initialization spawns nothing; execute_matrix marks uncertainty before
    # launching anything. No TemporaryDirectory finalizer may erase live state.
    evidence["processes_stopped"] = True
    try:
        execute_matrix(args, work, evidence)
        require(evidence["processes_stopped"], "owned process exit unconfirmed")
    finally:
        if evidence["processes_stopped"]:
            shutil.rmtree(work)
        else:
            evidence["private_state_retained"] = str(work)
            evidence["retention"] = "private state/logs retained because owned process exit is unconfirmed"


def interrupted(_signal, _frame):
    raise KeyboardInterrupt


def main():
    args = arguments()
    evidence = {"schema": 1, "status": "FAIL", "phase": "preflight", "results": [],
                "input_path": "SDK widget-key (implicitly focuses view); native commands/widget-click",
                "fixture_identity": "coordinator ResourceId + controlled OSC title + native textbox",
                "os_key_focus_acceptance": "not exercised", "snapshot_body_provenance": "unsupported",
                "retention": "results/counters/provenance only; payloads and ephemeral logs discarded"}
    output = ROOT / ".dev-run/input-roundtrip"
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    report = output / f"{time.time_ns()}-{secrets.token_hex(4)}.json"
    signal.signal(signal.SIGTERM, interrupted)
    scratch = Path(tempfile.gettempdir()) / "opencode"
    try:
        with identity.exclusive(output / "run.lock"):
            private_matrix(args, scratch, evidence)
        evidence["status"] = "PASS"
    except (Exception, KeyboardInterrupt) as error:
        # Never serialize str(SubprocessError): it includes argv and payloads.
        evidence["failure_kind"] = type(error).__name__
        if isinstance(error, Failure):
            evidence["refusal"] = str(error)
    identity.write_json(report, evidence)
    print(json.dumps({"status": evidence["status"], "phase": evidence["phase"], "report": str(report)}))
    return 0 if evidence["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
