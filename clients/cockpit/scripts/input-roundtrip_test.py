#!/usr/bin/env python3
"""Offline isolation/identity/refusal tests; never launches native, Phux or AppKit."""

import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from lib import input_roundtrip as helpers
from lib import agent_attention as agents

SPEC = importlib.util.spec_from_file_location("roundtrip_cli", Path(__file__).with_name("input-roundtrip.py"))
cli = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cli)


def inventory(*ids):
    return {"unreachable": [], "terminals": list(ids),
            "resources": [{"id": i, "kind": "terminal"} for i in ids]}


def textbox(title="rt-fixture-r4", view="@w1/phux-cockpit-canvas"):
    return {"name": title, "view": view, "role": "textbox", "id": "71",
            "focused": "true", "enabled": "true"}


class IsolationTests(unittest.TestCase):
    def test_state_destination_is_within_sdk_private_app_root(self):
        with tempfile.TemporaryDirectory() as d:
            env = helpers.prepare_environment(Path(d))
            allowed = Path(env["TMPDIR"]) / "dev.phux.cockpit"
            state = Path(env["PHUX_COCKPIT_STATE"])
            self.assertTrue(state.is_relative_to(allowed), "SDK refuses debounced state write")
            self.assertTrue(state.parent.is_dir())

    def test_scrubbed_environment_reaches_child_without_inherited_payload_knobs(self):
        hostile = {"PATH": os.environ["PATH"], "HOME": "/private-real-home",
                   "PHUX_SOCKET": "/live.sock", "PHUX_PROFILE": "live",
                   "PHUX_LOG": "/live.log", "PHUX_NEW_UNDOCUMENTED_SWITCH": "secret",
                   "PHUX_COCKPIT_TABS": "private", "NATIVE_SDK_PATH": "/wrong-sdk",
                   "ZDOTDIR": "/real-zsh", "BASH_ENV": "/real-hook",
                   "RUST_LOG": "trace", "DYLD_INSERT_LIBRARIES": "/inject"}
        with tempfile.TemporaryDirectory() as d:
            env = helpers.isolated_environment(Path(d), hostile)
            raw = subprocess.check_output(["/usr/bin/env"], env=env, text=True)
            for forbidden in ("secret", "/live", "/private-real-home", "/wrong-sdk", "/real-", "/inject", "trace"):
                self.assertNotIn(forbidden, raw)
            for key in ("HOME", "PHUX_SOCKET", "PHUX_LOG", "PHUX_COCKPIT_CONFIG", "PHUX_COCKPIT_STATE",
                        "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_RUNTIME_DIR", "ZDOTDIR"):
                self.assertTrue(Path(env[key]).is_relative_to(d), key)

    def test_symlinked_cross_checkout_artifact_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "checkout"
            root.mkdir()
            sibling = Path(d) / "other-native"
            sibling.write_text("artifact")
            (root / "native").symlink_to(sibling)
            with self.assertRaisesRegex(helpers.Failure, "outside this checkout"):
                helpers.checkout_artifact(root / "native", root)

    def test_refuses_existing_publisher_before_any_spawn(self):
        with patch.object(helpers.Path, "cwd", return_value=helpers.ROOT), \
             patch.object(helpers.identity, "live_publishers", return_value=[128]), \
             patch.object(helpers.subprocess, "Popen") as spawn:
            with self.assertRaisesRegex(helpers.Failure, "publisher is running"):
                helpers.preflight(Path("native"), Path("phux"), Path("ffi"))
            spawn.assert_not_called()

    def test_failed_command_does_not_expose_argv_or_stderr(self):
        with self.assertRaises(helpers.Failure) as caught:
            helpers.run(["/bin/sh", "-c", "echo private-payload >&2; exit 1"])
        self.assertNotIn("private-payload", str(caught.exception))

    def test_rebound_socket_refused_even_while_owned_server_pid_survives(self):
        with tempfile.TemporaryDirectory(prefix="rt-") as d:
            path = Path(d) / "s"
            with socket.socket(socket.AF_UNIX) as first, socket.socket(socket.AF_UNIX) as second:
                first.bind(str(path))
                launcher = helpers.Launcher.__new__(helpers.Launcher)
                launcher.env = {"PHUX_SOCKET": str(path)}
                launcher.socket_identity = helpers.identity.file_stamp(path.stat())
                launcher.server = Mock(pid=50)
                launcher.server.poll.return_value = None
                launcher.server_identity = {"pid": 50}
                launcher.check_inputs = Mock()
                path.unlink()
                second.bind(str(path))
                with patch.object(helpers.identity, "process_identity", return_value={"pid": 50}):
                    with self.assertRaisesRegex(helpers.Failure, "socket identity changed"):
                        launcher.check_server()

    def test_replaced_publisher_is_never_signalled_during_cleanup(self):
        launcher = helpers.Launcher.__new__(helpers.Launcher)
        launcher.app = {"pid": 128, "started": "old"}
        launcher.dev = Mock()
        with patch.object(helpers, "process_running", return_value=True), \
             patch.object(helpers.identity, "process_identity", return_value={"pid": 128, "started": "new"}), \
             patch.object(helpers.os, "kill") as kill, patch.object(helpers, "stop_child") as stop:
            with self.assertRaisesRegex(helpers.Failure, "replaced publisher"):
                launcher.stop_app()
            kill.assert_not_called()
            stop.assert_not_called()


class IdentityTests(unittest.TestCase):
    def test_mixed_inventory_keeps_only_terminal_facet_targets(self):
        report = inventory("@1", "@2")
        report["resources"].append({"id": "@3", "kind": "agent_session", "parent": "@1"})
        self.assertEqual(helpers.terminal_ids(report), {"@1", "@2"})
        self.assertEqual(helpers.resource_inventory(report)["@3"], {"kind": "agent_session", "parent": "@1"})

    def test_nonterminal_rows_are_validated_before_filtering(self):
        for agent_id in ("@1", "remote/@3", "@0", "@\u0663"):
            report = inventory("@1", "@2")
            report["resources"].append({"id": agent_id, "kind": "agent_session", "parent": "@1"})
            with self.subTest(agent_id=agent_id), self.assertRaises(helpers.Failure):
                helpers.terminal_ids(report)

    def test_mixed_inventory_terminal_facet_must_agree_exactly(self):
        for terminals in (["@1", "@2", "@3"], ["@1"], ["@1", "@2", "@2"]):
            report = inventory("@1", "@2")
            report["resources"].append({"id": "@3", "kind": "agent_session", "parent": "@1"})
            report["terminals"] = terminals
            with self.subTest(terminals=terminals), self.assertRaises(helpers.Failure):
                helpers.terminal_ids(report)

    def test_resource_delta_uses_exact_id_not_tab_ordinal(self):
        before = helpers.terminal_ids(inventory("@1", "@2"))
        after = helpers.terminal_ids(inventory("@1", "@2", "@4"))
        self.assertEqual(helpers.added_terminal(before, after), "@4")

    def test_ambiguous_and_partial_inventories_refused(self):
        bad = inventory("@4")
        bad["unreachable"] = ["host"]
        cases = [bad, inventory("@4", "@4"), inventory("host/@4")]
        nonterminal = inventory("@4")
        nonterminal["resources"][0]["kind"] = "agent_session"
        cases.append(nonterminal)
        for report in cases:
            with self.subTest(report=report), self.assertRaises(helpers.Failure):
                helpers.terminal_ids(report)
        with self.assertRaises(helpers.Failure):
            helpers.added_terminal({"@1"}, {"@1", "@4", "@5"})
        with self.assertRaises(helpers.Failure):
            helpers.added_terminal({"@1"}, {"@4"})

    def test_fixture_requires_exact_title_role_and_unique_owner(self):
        raw = (b'    widget @w1/phux-cockpit-canvas#71 role=textbox name="rt-fixture-r4" '
               b'bounds=(1,2 30x40) focused=true enabled=true parent=5\n')
        widgets = helpers.fixture_widgets(raw)
        widget = helpers.find_widget(widgets, "textbox", "rt-fixture-r4")
        helpers.require_owner(widget, "rt-fixture-r4", "@w1/phux-cockpit-canvas")
        self.assertIsNone(helpers.find_widget(widgets, "textbox", "rt-fixture-r3"))
        with self.assertRaisesRegex(helpers.Failure, "ambiguous"):
            helpers.find_widget(widgets + widgets, "textbox", "rt-fixture-r4")

    def test_wrong_resource_title_or_view_refused_before_input(self):
        probe = cli.Probe(Mock(), Mock(), {"results": []})
        probe.titles = {"@4": "rt-fixture-r4"}
        probe.automate = Mock()
        for widget in (textbox(title="rt-fixture-r3"), textbox(view="@w2/phux-cockpit-canvas-1")):
            probe.textbox = Mock(return_value=widget)
            with self.subTest(widget=widget), self.assertRaises(helpers.Failure):
                probe.key("@4", "@w1/phux-cockpit-canvas", "enter")
        probe.automate.assert_not_called()

    def test_unfocused_owner_refused_before_sdk_can_focus_view(self):
        probe = cli.Probe(Mock(), Mock(), {"results": []})
        probe.owner = Mock(return_value={**textbox(), "focused": "false"})
        probe.key = Mock()
        with self.assertRaisesRegex(helpers.Failure, "not focused"):
            probe.roundtrip("toolbar-create", "@4", "@w1/phux-cockpit-canvas")
        probe.key.assert_not_called()

    def test_publisher_replacement_refused_without_input(self):
        launcher = Mock()
        launcher.check_app.side_effect = helpers.Failure("publisher executable or start time changed")
        probe = cli.Probe(launcher, Mock(), {})
        with patch.object(cli, "run") as command:
            with self.assertRaises(helpers.Failure):
                probe.automate("widget-key", "@w1/phux-cockpit-canvas", "enter")
            command.assert_not_called()


class FailureRetentionTests(unittest.TestCase):
    def test_unconfirmed_cleanup_preserves_private_state(self):
        with tempfile.TemporaryDirectory() as d:
            work = []

            def failed(_args, path, evidence):
                work.append(path)
                (path / "layout.json").write_text("private fixture")
                evidence["processes_stopped"] = False
                raise helpers.Failure("owned process exit unconfirmed")

            with patch.object(cli, "arguments", return_value=Mock()), \
                 patch.object(cli, "ROOT", Path(d)), \
                 patch.object(cli, "execute_matrix", side_effect=failed), \
                 patch("builtins.print"):
                self.assertEqual(cli.main(), 1)
            try:
                self.assertTrue((work[0] / "layout.json").exists(), "unconfirmed app lost its state")
            finally:
                shutil.rmtree(work[0], ignore_errors=True)

    def test_timeout_retains_failure_without_command_payload(self):
        with tempfile.TemporaryDirectory() as d:
            args = Mock()
            timeout = subprocess.TimeoutExpired(["native", "private-input"], 20, output=b"private-screen")
            with patch.object(cli, "arguments", return_value=args), \
                 patch.object(cli, "ROOT", Path(d)), \
                 patch.object(cli, "execute_matrix", side_effect=timeout), \
                 patch("builtins.print"):
                self.assertEqual(cli.main(), 1)
            paths = list((Path(d) / ".dev-run/input-roundtrip").glob("*.json"))
            self.assertEqual(len(paths), 1)
            raw = paths[0].read_text()
            self.assertNotIn("private-input", raw)
            self.assertNotIn("private-screen", raw)
            self.assertEqual(json.loads(raw)["failure_kind"], "TimeoutExpired")


class CleanupTests(unittest.TestCase):
    def test_unconfirmed_kill_keeps_cleanup_unconfirmed(self):
        launcher = helpers.Launcher.__new__(helpers.Launcher)
        launcher.app = {"pid": 128, "started": "owned"}
        launcher.app_shutdown_confirmed = False
        launcher.dev = launcher.server = None
        launcher.evidence = {}
        evidence = {}
        with patch.object(helpers, "process_running", return_value=True), \
             patch.object(helpers.identity, "process_identity", return_value=launcher.app), \
             patch.object(helpers, "wait_for", side_effect=helpers.WaitTimeout("timeout")), \
             patch.object(helpers.os, "kill") as kill:
            with self.assertRaises(helpers.Failure):
                cli.close_matrix(launcher, None, evidence)
        self.assertEqual(kill.call_count, 2)
        self.assertIsNotNone(launcher.app)
        self.assertFalse(evidence["processes_stopped"])

    def test_replacement_before_escalation_is_not_killed(self):
        launcher = helpers.Launcher.__new__(helpers.Launcher)
        launcher.app = {"pid": 128, "started": "owned"}
        launcher.dev = Mock()
        with patch.object(helpers, "process_running", return_value=True), \
             patch.object(helpers.identity, "process_identity", side_effect=[launcher.app, {"pid": 128, "started": "replaced"}]), \
             patch.object(helpers, "wait_for", side_effect=helpers.WaitTimeout("timeout")), \
             patch.object(helpers.os, "kill") as kill, patch.object(helpers, "stop_child") as stop:
            with self.assertRaisesRegex(helpers.Failure, "replaced publisher"):
                launcher.stop_app()
            kill.assert_called_once_with(128, helpers.signal.SIGTERM)
            stop.assert_not_called()

    def stubborn_child(self):
        child = subprocess.Popen([sys.executable, "-c",
            "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            "print('ready', flush=True); time.sleep(60)"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        self.assertEqual(child.stdout.readline(), b"ready\n")
        self.addCleanup(child.stdout.close)
        self.addCleanup(child.stdin.close)
        self.addCleanup(lambda: self.reap(child))
        return child

    @staticmethod
    def reap(child):
        if child.poll() is None:
            child.kill()
        child.wait(timeout=2)

    def test_hung_owned_app_is_escalated_and_exit_confirmed(self):
        child = self.stubborn_child()
        launcher = helpers.Launcher.__new__(helpers.Launcher)
        launcher.app = {"pid": child.pid, "started": "owned"}
        launcher.dev = None
        original_wait = helpers.wait_for
        with patch.object(helpers.identity, "live_publishers", side_effect=lambda: [child.pid] if child.poll() is None else []), \
             patch.object(helpers.identity, "process_identity", return_value=launcher.app), \
             patch.object(helpers, "wait_for", side_effect=lambda f, d, *a: original_wait(f, d, 0.2)):
            try:
                launcher.stop_app()
            except helpers.Failure:
                pass
        self.assertIsNotNone(child.poll(), "SIGTERM timeout left owned app running")
        self.assertIsNone(launcher.app)

    def test_hung_appkit_helper_is_killed_and_reaped(self):
        child = self.stubborn_child()
        appkit = helpers.AppKit.__new__(helpers.AppKit)
        appkit.child = child
        appkit.request = Mock()
        original_wait = child.wait
        with patch.object(child, "wait", side_effect=lambda timeout: original_wait(timeout=0.2)):
            try:
                appkit.close()
            except (helpers.Failure, subprocess.TimeoutExpired):
                pass
        self.assertIsNotNone(child.poll(), "helper retained saved clipboard after close")


class RestartTests(unittest.TestCase):
    def test_partial_state_is_not_a_persisted_effect(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "state"
            for partial in (b"", b"phux-cockpit-state 5\nplacement top\n", b"garbage\nend\n"):
                path.write_bytes(partial)
                self.assertFalse(helpers.persisted_state_effect(path))

    def probe(self, work, views):
        state = Path(work) / "layout.state"
        state.write_text("phux-cockpit-state 5\nplacement top\nend\n")
        launcher = Mock(app={"pid": 50}, env={"PHUX_COCKPIT_STATE": str(state)})
        launcher.start_app.side_effect = lambda: setattr(launcher, "app", {"pid": 51})
        probe = cli.Probe(launcher, Mock(), {"results": []})
        probe.groups = [["@1"], ["@2"], ["@3"]]
        probe.window_groups = [["@1", "@2"], ["@3"]]
        probe.titles = {t: f"rt-fixture-r{t[1:]}" for t in views}
        probe.inventory = lambda: set(views)
        probe.screen = lambda t: {"title": probe.titles[t]}
        probe.textbox = lambda t: textbox(probe.titles[t], views[t])

        def widget(role, title, view=None):
            target = next(t for t in views if probe.titles[t] == title)
            return {**textbox(title, views[target]), "role": role}

        probe.widget = widget
        probe.click = Mock()
        probe.await_publication = Mock()
        probe.roundtrip = Mock()
        return probe, state

    def test_shutdown_only_state_cannot_satisfy_persistence_acceptance(self):
        with tempfile.TemporaryDirectory() as d:
            probe, state = self.probe(d, {"@1": "@w7/canvas", "@2": "@w7/canvas", "@3": "@w9/canvas"})
            state.unlink()
            probe.launcher.stop_app.side_effect = lambda: state.write_text("phux-cockpit-state 5\nplacement top\nend\n")
            original_wait = helpers.wait_for
            with patch.object(cli, "wait_for", side_effect=lambda f, d, *a: original_wait(f, d, 0.2)):
                with self.assertRaises(helpers.Failure):
                    probe.restart()
            probe.launcher.stop_app.assert_not_called()

    def test_secondary_collapsed_into_main_cannot_pass_restart(self):
        with tempfile.TemporaryDirectory() as d:
            probe, _ = self.probe(d, {t: "@w7/canvas" for t in ("@1", "@2", "@3")})
            with self.assertRaisesRegex(helpers.Failure, "window"):
                probe.restart()

    def test_main_tabs_split_across_windows_cannot_pass_restart(self):
        with tempfile.TemporaryDirectory() as d:
            probe, _ = self.probe(d, {"@1": "@w7/canvas", "@2": "@w8/canvas", "@3": "@w9/canvas"})
            with self.assertRaisesRegex(helpers.Failure, "window"):
                probe.restart()

    def test_restart_accepts_new_ids_with_same_window_relationships(self):
        with tempfile.TemporaryDirectory() as d:
            probe, _ = self.probe(d, {"@1": "@w7/canvas", "@2": "@w7/canvas", "@3": "@w9/canvas"})
            probe.restart()
            self.assertEqual(probe.roundtrip.call_count, 3)
            self.assertTrue(probe.evidence["state_effect_before_shutdown"])
            self.assertEqual(probe.evidence["restart_window_groups"], [
                {"targets": ["@1", "@2"], "window": "@w7"},
                {"targets": ["@3"], "window": "@w9"},
            ])


class AgentFixture:
    """Offline UI/producer model behind the real Probe click/key/owner methods.

    Faults alter observed behavior, not the acceptance assertions. No native,
    Phux, fixture subprocess or AppKit is invoked by this model.
    """
    view = "@w1/phux-cockpit-canvas"

    def __init__(self, work, fault=None):
        self.work, self.fault = work, fault
        self.focused, self.side, self.inspector = "@2", False, False
        self.selected_terminal = "@2"
        self.parent_attention = False
        self.placement_changes = []
        self.live, self.retained_row = False, False
        self.state, self.attention, self.stage = "unknown", False, "shell"
        self.typed, self.buffer, self.records = [], "", []
        self.screens = {"@1": [], "@2": [], "@3": []}
        self.native_id, self.resource, self.parent = "", "@4", "@1"
        self.seq, self.ts, self.reason, self.kind = 0, 0, "", "ask"
        (work / "scripts").mkdir()
        (work / "scripts/agent-attention-proof.py").touch()
        launcher = Mock(work=work, app={"pid": 50, "started_unix": 1},
                        env={"PHUX_SOCKET": str(work / "p.sock")})
        launcher.args.phux = work / "phux"
        self.probe = cli.Probe(launcher, Mock(), {"results": []})
        self.probe.titles = {t: f"rt-fixture-r{t[1:]}" for t in self.screens}
        self.probe.server = self.server
        self.probe.snapshot = self.snapshot
        self.probe.automate = self.automate

    def server(self, verb, target=None):
        if verb == "snapshot":
            return {"title": self.probe.titles[target], "lines": self.screens[target]}
        assert verb == "ls"
        return self.catalog()

    def catalog(self):
        report = inventory(*self.screens)
        if self.live:
            report["resources"].append({"id": self.resource, "kind": "agent_session", "parent": self.parent})
        if self.fault == "extra-resource" and self.stage == "answer":
            report["resources"].append({"id": "@9", "kind": "agent_session", "parent": "@2"})
        return report

    def add_widget(self, role, name, target=None):
        widget = {"role": role, "name": name, "id": str(len(self.widgets) + 1), "view": self.view,
                  "focused": str(target == self.focused).lower(), "enabled": "true"}
        self.widgets.append(widget)

    def snapshot(self):
        self.widgets = []
        self.add_widget("tab", self.probe.titles[self.selected_terminal])
        self.parent_attention_widget()
        for target in ("@1", "@2"):
            self.add_widget("textbox", self.probe.titles[target], target)
        self.add_widget("button", "Agents " + str(int(self.live)))
        self.rail_widgets()
        if self.inspector:
            self.inspection_widgets()
        return self.serialized_widgets()

    def parent_attention_widget(self):
        if self.fault == "missing-parent-top" and not self.side:
            return
        if self.parent_attention:
            self.add_widget("text", "Needs attention: " + self.probe.titles[self.selected_terminal])

    def rail_widgets(self):
        if self.side and (self.live or self.retained_row):
            self.add_widget("button", f"cockpit-proof / phux:0:4@ under phux:0:{self.parent[1:]}@")
            self.add_widget("text", self.state)
            if self.attention:
                self.add_widget("text", "\u25cf")

    def serialized_widgets(self):
        # Exercise the controlled parser against the SDK's literal multiline
        # label format rather than handing the driver pre-parsed evidence.
        raw = "\n".join(f'    widget {w["view"]}#{w["id"]} role={w["role"]} name="{w["name"]}" '
                        f'bounds=(0,0 10x10) focused={w["focused"]} enabled=true parent=#1'
                        for w in self.widgets).encode()
        return helpers.fixture_widgets(raw), {"publisher_pid": "50"}

    def inspection_widgets(self):
        self.add_widget("button", "Close agent inspector")
        if not self.live:
            self.add_widget("text", "No agent resources in the attached catalog")
            return
        self.add_widget("button", "Jump to parent")
        self.add_widget("text", "phux:0:4@")
        self.add_widget("text", f"phux:0:{self.parent[1:]}@")
        self.add_widget("text", self.native_id)
        seq = self.seq - 1 if self.fault == "stale-ui-sequence" else self.seq
        reason = "Preparing terminal intervention proof" if self.fault == "stale-second-reason" else self.reason
        self.add_widget("text", f"Provider: cockpit-proof\nCatalog: unknown; records: {self.state}\n"
                        f"Latest record: {self.kind}\nSequence: {seq}\n"
                        f"Coordinator-stamped record time (ts_ms): {self.ts}\nReason: {reason}")

    def automate(self, action, *args):
        actions = {"native-command": self.toggle_placement, "widget-click": self.click_id,
                   "widget-key": self.widget_key}
        actions[action](*args)

    def click_id(self, view, widget_id):
        assert view == self.view
        self.click(next(w for w in self.widgets if w["id"] == widget_id))

    def widget_key(self, view, key, text=None):
        assert view == self.view
        self.key(key, text)

    def toggle_placement(self, command, view):
        assert (command, view) == ("tabs.toggle-placement", self.view)
        self.side = not self.side
        self.placement_changes.append((self.side, self.stage, len(self.typed)))

    def click(self, widget):
        name = widget["name"]
        if widget["role"] in ("tab", "textbox"):
            self.focused = next(t for t, title in self.probe.titles.items() if title == name)
            self.selected_terminal = self.focused
            return
        self.click_inspector(name)

    def click_inspector(self, name):
        if name.startswith("Agents "):
            self.inspector, self.focused = True, None
            self.preserve_attention()
        elif name == "Close agent inspector":
            self.inspector = False
        elif name == "Jump to parent":
            self.inspector = False
            self.focused = "@2" if self.fault == "jump-wrong-split" else "@1"
            self.selected_terminal = self.focused

    def preserve_attention(self):
        if self.fault == "inspection-clears-attention":
            self.attention = False

    def key(self, key, text=None):
        assert self.focused is not None
        if text is not None:
            self.input_text(text)
        if key == "enter":
            self.typed.append(self.buffer)
            action = {"shell": self.open, "inspect": self.second_ask,
                      "answer": self.answer, "close": self.close}[self.stage]
            action(self.buffer)
            self.buffer = ""

    def input_text(self, text):
        self.buffer += text
        if self.fault == "premature-result" and self.stage == "answer":
            self.screens["@1"].append(str(int(text) + 451))

    def receipt(self, value):
        self.records.append(value)
        with (self.work / "agent-receipt.jsonl").open("a") as stream:
            stream.write(json.dumps(value) + "\n")

    def stamp(self, phase, seq, kind, reason, state):
        self.seq, self.ts, self.kind, self.reason, self.state = seq, 2000 + seq, kind, reason, state
        self.attention = state == "blocked"
        self.parent_attention = self.attention or self.fault == "retained-parent-attention"
        return {"phase": phase, "resource": self.resource, "seq": seq, "ts_ms": self.ts, "type": kind}

    def open(self, command):
        args = shlex.split(command)
        self.native_id = args[args.index("--run-id") + 1]
        if self.fault == "wrong-native-id":
            self.native_id = "different-producer"
        if self.fault == "wrong-parent":
            self.parent = "@2"
        self.live, self.stage = True, "inspect"
        self.receipt({"phase": "opened", "resource": self.resource, "parent": self.parent,
                      "native_id": self.native_id, "provider": "cockpit-proof"})
        self.receipt(self.stamp("blocked-initial", 1, "ask", "Preparing terminal intervention proof", "blocked"))

    def second_ask(self, command):
        assert command == "inspect"
        self.stage = "answer"
        self.receipt(self.stamp("blocked", 2, "ask", "Enter a decimal proof value in this terminal", "blocked"))

    def answer(self, command):
        assert command.isdecimal()
        self.stage = "close"
        result = str(int(command) + 451)
        self.screens["@1"].append(result)
        if self.fault == "duplicate-result":
            self.screens["@2"].append(result)
        self.receipt({**self.stamp("done", 4, "stop", "Terminal intervention completed", "done"),
                      "computed_result": result})

    def close(self, command):
        assert command == "close"
        self.live = False
        self.parent_attention = False
        self.retained_row = self.fault == "retained-row"
        self.receipt({"phase": "closed", "resource": self.resource, "parent": self.parent})


class AgentAcceptanceTests(unittest.TestCase):
    def test_matrix_runs_agent_workflow_once_before_restart(self):
        probe = Mock(groups=[], window_groups=[["@1"]], evidence={})
        probe.start.return_value = ("@1", AgentFixture.view)
        probe.transition.side_effect = [("@2", AgentFixture.view), ("@3", AgentFixture.view),
                                        ("@5", "@w2/phux-cockpit-canvas-1")]
        order = []
        probe.restart.side_effect = lambda: order.append("restart")
        with patch.object(cli, "AgentAcceptance") as driver, \
             patch.object(cli.identity, "source_identity", return_value={}):
            driver.return_value.run.side_effect = lambda: order.append("agent")
            cli.Probe.matrix(probe)
        driver.assert_called_once_with(probe, "@2", "@3", AgentFixture.view)
        self.assertEqual(order, ["agent", "restart"])

    @staticmethod
    def immediate(check, description, *unused):
        result = check()
        if not result:
            raise helpers.WaitTimeout("timeout: " + description)
        return result

    def run_fixture(self, fixture):
        with patch.object(agents, "ROOT", fixture.work), \
             patch.object(agents, "wait_for", side_effect=self.immediate), \
             patch.object(cli, "wait_for", side_effect=self.immediate):
            driver = agents.AgentAcceptance(fixture.probe, "@1", "@2", fixture.view)
            driver.run()

    def test_full_serial_agent_choreography_and_private_report(self):
        with tempfile.TemporaryDirectory() as d:
            fixture = AgentFixture(Path(d))
            self.run_fixture(fixture)
            self.assertEqual(fixture.typed[1], "inspect")
            self.assertTrue(fixture.typed[2].isdecimal())
            self.assertEqual(fixture.typed[3], "close")
            self.assertFalse(fixture.live)
            self.assertFalse(fixture.side, "driver must restore top tabs before restart")
            self.assertEqual(fixture.placement_changes, [(True, "inspect", 1), (False, "close", 4)],
                             "first ask must be observed in default top tabs before switching to the rail")
            report = json.dumps(fixture.probe.evidence)
            for private in (fixture.typed[0], fixture.typed[2], fixture.screens["@1"][0], d,
                            "Preparing terminal intervention proof", "Enter a decimal proof value"):
                self.assertNotIn(private, report)
            self.assertEqual([r["phase"] for r in fixture.probe.evidence["results"]], [
                "agent-birth", "agent-blocked-initial", "agent-readonly-jump", "agent-blocked",
                "agent-intervention", "agent-retirement"])
            self.assertEqual([r["tab_placement"] for r in fixture.probe.evidence["results"]],
                             ["top", "top", "top", "side", "side", "top"])

    def test_agent_behavioral_faults_cannot_pass_acceptance(self):
        faults = ("wrong-parent", "wrong-native-id", "stale-ui-sequence", "stale-second-reason",
                  "inspection-clears-attention", "jump-wrong-split", "extra-resource",
                  "premature-result", "duplicate-result", "retained-row")
        for fault in faults:
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as d:
                fixture = AgentFixture(Path(d), fault)
                with self.assertRaises(helpers.Failure):
                    self.run_fixture(fixture)
                self.assertNotIn("agent-retirement", [r["phase"] for r in fixture.probe.evidence["results"]])

    def test_top_parent_attention_required_even_when_agent_rail_dot_exists(self):
        with tempfile.TemporaryDirectory() as d:
            fixture = AgentFixture(Path(d), "missing-parent-top")
            with self.assertRaisesRegex(helpers.Failure, "parent tab attention"):
                self.run_fixture(fixture)

    def test_done_must_clear_parent_attention_even_when_agent_dot_clears(self):
        with tempfile.TemporaryDirectory() as d:
            fixture = AgentFixture(Path(d), "retained-parent-attention")
            with self.assertRaisesRegex(helpers.Failure, "parent tab attention"):
                self.run_fixture(fixture)

    def test_receipt_partial_write_order_and_budget(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "receipt"
            self.assertEqual(agents.read_receipts(path), [])
            path.write_text('{"phase":"opened"}\n{"phase":')
            self.assertEqual(agents.read_receipts(path), [{"phase": "opened"}])
            for invalid in ('{"phase":"done"}\n', 'x' * 16385):
                path.write_text(invalid)
                with self.assertRaises(helpers.Failure):
                    agents.read_receipts(path)

    def test_stamped_evidence_requires_exact_resource_and_ordered_integers(self):
        record = {"phase": "blocked", "resource": "@4", "seq": 2, "ts_ms": 100, "type": "ask"}
        for bad in ({"resource": "@5"}, {"seq": True}, {"seq": 1}, {"ts_ms": 9},
                    {"type": "stop"}, {"ts_ms": 2**64}):
            with self.subTest(bad=bad), self.assertRaises(helpers.Failure):
                agents.stamped_record({**record, **bad}, "@4", {"seq": 1, "ts_ms": 10})


class RoutingTests(unittest.TestCase):
    def fixture_probe(self, extra_delivery=False, submit_early=False, spawn_on_enter=False):
        probe = cli.Probe(Mock(app={"pid": 50}), Mock(), {"results": []})
        probe.owner = Mock(return_value=textbox())
        screens = {"@4": [], "@7": []}
        ids = {"@4", "@7"}
        probe.inventory = lambda: set(ids)
        probe.screen = lambda target: {"lines": screens[target]}
        probe.snapshot = lambda: ([], {"publisher_pid": "50"})

        def key(target, view, key, text=None):
            if text is not None:
                screens[target].append(text)
                if submit_early:
                    screens[target].append("100000042")
            if key == "enter":
                screens[target].append("100000042")
                if extra_delivery:
                    screens["@7"].append("100000042")
                if spawn_on_enter:
                    ids.add("@8")
        probe.key = key
        return probe

    @patch.object(cli.secrets, "randbelow", return_value=42)
    def test_success_reports_id_and_counters_without_input_or_result_text(self, _random):
        probe = self.fixture_probe()
        probe.roundtrip("toolbar-create", "@4", "@w1/phux-cockpit-canvas")
        report = json.dumps(probe.evidence)
        self.assertNotIn("100000042", report)
        self.assertNotIn("printf", report)
        self.assertEqual(probe.evidence["results"][0]["target"], "@4")
        self.assertEqual(probe.evidence["results"][0]["other_ptys_checked"], 1)

    @patch.object(cli.secrets, "randbelow", return_value=42)
    def test_duplicate_delivery_premature_submit_and_toolbar_enter_cannot_pass(self, _random):
        for mode in ("extra_delivery", "submit_early", "spawn_on_enter"):
            probe = self.fixture_probe(**{mode: True})
            with self.subTest(mode=mode), self.assertRaises(helpers.Failure):
                probe.roundtrip("toolbar-create", "@4", "@w1/phux-cockpit-canvas")
            self.assertEqual(probe.evidence["results"], [])


if __name__ == "__main__":
    unittest.main()
