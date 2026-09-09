#!/usr/bin/env python3
"""Offline isolation/identity/refusal tests; never launches native, Phux or AppKit."""

import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from lib import input_roundtrip as helpers

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
        with patch.object(helpers.identity, "live_publishers", return_value=[128]), \
             patch.object(helpers.identity, "process_identity", return_value={"pid": 128, "started": "new"}), \
             patch.object(helpers.os, "kill") as kill, patch.object(helpers, "stop_child"):
            with self.assertRaisesRegex(helpers.Failure, "replaced publisher"):
                launcher.stop_app()
            kill.assert_not_called()


class IdentityTests(unittest.TestCase):
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
