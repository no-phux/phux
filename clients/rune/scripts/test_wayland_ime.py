"""Executable fake-platform tests; these are not Linux/app qualification."""

import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from wayland_ime_fixture import DRIVER, WaylandImeFixture

SPEC = importlib.util.spec_from_file_location("wayland_ime", DRIVER)
ime = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ime)

class WaylandImeTests(WaylandImeFixture, unittest.TestCase):
    def test_single_ascii_composition_with_real_echo_and_unrestricted_preedit(self):
        result, calls, stages, files = self.run_driver()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c for c in calls if c[0] == "wtype"], [ime.KEYS])
        self.assertIn(["fcitx5-remote", "-o"], calls)
        self.assertIn("set_cursor_rectangle", files["wayland-ime-app.log"])
        self.assertIn('done(47)', files["wayland-ime-app.log"])
        self.assertIn("ni’hao", files["wayland-ime-app.log"])
        self.assertIn("full snapshot row", files["wayland-ime-terminal.log"])
        self.assertEqual(sum(s["stage"] == "process-reaped" for s in stages), 2)
        self.assertEqual(sum(s["stage"] == "injection-start" for s in stages), 1)
        self.assertIn("wayland-ime-bus-owner.log", files)
        self.assertIn("wayland-ime-tree-before.json", files)

    def test_each_command_records_work_within_its_supplied_budget(self):
        result, _, stages, _ = self.run_driver()
        self.assertEqual(result.returncode, 0, result.stderr)
        starts = [s for s in stages if s["stage"] == "command-start"]
        exits = [s for s in stages if s["stage"] == "command-exit"]
        self.assertEqual(len(starts), len(exits))
        for start, end in zip(starts, exits):
            self.assertEqual(start["argv"], end["argv"])
            self.assertAlmostEqual(end["elapsed"] - start["elapsed"], 0.25)
            self.assertLessEqual(end["timeout"], start["timeout_cap"])

    def test_wrong_owners_profile_only_and_failed_commands_never_inject(self):
        cases = {
            "wrong-focus": "keyboard focus", "wrong-app-id": "keyboard focus",
            "no-context": "focused Rune Wayland context", "split-context": "focused Rune",
            "wrong-frontend": "focused Rune", "profile-only": "effective Pinyin",
            "inactive": "effective Pinyin", "no-bus": "owns org.fcitx.Fcitx5",
            "wrong-bus-pid": "owner PID", "stale-session": "stale session",
            "activation-fails": "command failed (4)",
            "tree-command-fails": "command failed (6)",
            "dead-rune-fake": "app exited (7)", "dead-fcitx5": "fcitx exited (7)",
            "hung-tree": "restarted Rune PID has keyboard focus",
        }
        for mode, message in cases.items():
            with self.subTest(mode=mode):
                result, calls, stages, files = self.run_driver(mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertFalse(any(c[0] == "wtype" for c in calls))
                self.assert_failure_evidence(stages, files)
                failed = next(event for event in stages if event["stage"] == "failure")
                self.assertLessEqual(failed["elapsed"], ime.STARTUP_SECONDS)
                if mode == "hung-tree":
                    self.assertTrue(any(
                        event["stage"] == "command-observation-failed"
                        and event["artifact"] == "wayland-ime-tree-before.json"
                        and event["status"] == "TimeoutExpired"
                        for event in stages
                    ))

    def test_no_replay_or_false_success_after_composition(self):
        for mode in ("typing-fails", "app-dies-during-composition", "no-commit",
                     "plain-ascii", "wrong-chinese", "no-echo", "empty-snapshot"):
            with self.subTest(mode=mode):
                result, calls, stages, files = self.run_driver(mode)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual([c for c in calls if c[0] == "wtype"], [ime.KEYS], result.stderr)
                self.assertTrue(any(s["stage"] == "failure-liveness" for s in stages))
                self.assertIn("wayland-ime-terminal-failure.log", files)
                if mode == "no-commit":
                    self.assertIn("no commit_string", result.stderr)

    def test_context_tokens_cannot_be_joined_across_input_contexts(self):
        self.assertFalse(ime.focused_context(
            "program:sh.phux.rune frontend:wayland_v2 focus:0\n"
            "program:other frontend:wayland_v2 focus:1"))
        self.assertFalse(ime.focused_context("program:sh.phux.rune frontend:wayland_v2 focus:10"))
        self.assertTrue(ime.focused_context(
            'string "IC program:sh.phux.rune frontend:wayland_v2 cap:52 focus:1"'))

    def test_startup_deadline_is_shared_and_exactly_nine_seconds(self):
        self.assertEqual(ime.STARTUP_SECONDS, 9)
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            with patch.object(ime.time, "monotonic", return_value=100):
                driver = ime.Diagnostic(directory, "fake-app", "fake-phux", "socket", "session")
            self.assertEqual(driver.deadline, 109)
            with patch.object(ime.time, "monotonic", return_value=108):
                driver.wait_for("first", lambda: True)
            self.assertEqual(driver.deadline, 109)
            with patch.object(ime.time, "monotonic", return_value=109):
                with self.assertRaisesRegex(RuntimeError, "timed out waiting for second"):
                    driver.wait_for("second", lambda: True)
                with self.assertRaisesRegex(RuntimeError, "before injection"):
                    driver.compose()
            self.assertEqual(driver.deadline, 109)

    def test_commands_are_clipped_to_remaining_deadline(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            with patch.object(ime.time, "monotonic", return_value=100):
                driver = ime.Diagnostic(directory, "fake-app", "fake-phux", "socket", "session")
            with patch.object(ime.time, "monotonic", return_value=108.75):
                with patch.object(ime.subprocess, "run") as run:
                    run.return_value.returncode = 0
                    driver.command(["fixture-only"], "bounded.log", timeout=20)
                    self.assertEqual(run.call_args.kwargs["timeout"], 0.25)
            with patch.object(ime.time, "monotonic", return_value=109):
                with patch.object(ime.subprocess, "run") as run:
                    with self.assertRaisesRegex(RuntimeError, "deadline expired"):
                        driver.command(["fixture-only"], "never-ran.log")
                    run.assert_not_called()

    def test_stage_and_command_logs_survive_existing_evidence_export(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            driver = ime.Diagnostic(directory, "fake-app", "fake-phux", "socket", "session")
            with patch.object(ime.subprocess, "run") as run:
                run.return_value.returncode = 0
                driver.command(["fixture-only"], "wayland-ime-tree-before.json")
            files = {p.name for p in Path(directory).iterdir()}
            self.assertEqual(files, {"wayland-ime-tree-before.json",
                                     "wayland-ime-tree-before.json.stderr.log",
                                     "wayland-ime-stages.log"})
            # Exercise qualify.sh's actual cp globs without changing that caller.
            export = (DRIVER.parent / "qualify.sh").read_text()
            for suffix in ("png", "json", "log"):
                self.assertIn(f'"$scratch"/*.{suffix}', export)

    def test_composition_has_one_shared_two_second_echo_budget(self):
        self.assertEqual(ime.ECHO_SECONDS, 2)
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
            driver = ime.Diagnostic(directory, "fake-app", "fake-phux", "socket", "session")
            driver.deadline = 209
            (Path(directory) / "wayland-ime-app.log").write_text(
                'zwp_text_input_v3@12.commit_string("你好")\n')
            with patch.object(ime.time, "monotonic", return_value=200):
                with patch.object(driver, "command") as command:
                    with patch.object(driver, "wait_for") as wait:
                        driver.compose()
            command.assert_called_once_with(ime.KEYS, "wayland-ime-wtype.log", timeout=5)
            wait.assert_called_once()
            self.assertEqual(driver.deadline, 202)

    def test_seat0_keyboard_bit_accepts_keyboard_with_other_capabilities(self):
        for capabilities in (2, 3, 6, 7):
            with self.subTest(capabilities=capabilities):
                reply = json.dumps([{"name": "seat0", "capabilities": capabilities}])
                self.assertTrue(ime.seat_keyboard_ready(reply))

    def test_pointer_touch_zero_and_wrong_seat_do_not_satisfy_keyboard_readiness(self):
        cases = [[], [{"name": "seat1", "capabilities": 2}]]
        for capabilities in (0, 1, 4):
            cases.append([{"name": "seat1", "capabilities": 2},
                          {"name": "seat0", "capabilities": capabilities}])
        for seats in cases:
            with self.subTest(seats=seats):
                self.assertFalse(ime.seat_keyboard_ready(json.dumps(seats)))

    def test_malformed_seat_schema_and_capability_types_fail_closed(self):
        replies = ["not JSON", "", "{}", "null", "2", "[null]", "[2]"]
        for capabilities in (None, True, "2", 2.0, -1):
            replies.append(json.dumps([{"name": "seat0", "capabilities": capabilities}]))
        replies.append('[{"name": "seat0"}]')
        replies.append('[{"name": "seat0", "capabilities": 2}, null]')
        for reply in replies:
            with self.subTest(reply=reply):
                with self.assertRaises(ValueError):
                    ime.seat_keyboard_ready(reply)

    def test_late_keyboard_is_observed_before_app_spawn(self):
        result, calls, stages, files = self.run_driver("late-seat")
        self.assertEqual(result.returncode, 0, result.stderr)
        observations = [json.loads(line) for line in
                        files["wayland-fixture-seats.log"].splitlines()]
        self.assertEqual([o["capabilities"] for o in observations], [0, 0, 2])
        self.assertTrue(all(not o["app_exists"] for o in observations))
        order = [(s["stage"], s.get("name")) for s in stages]
        self.assertLess(order.index(("seat-ready", None)), order.index(("process-start", "app")))
        self.assertEqual(json.loads(files["wayland-ime-seats-before.json"]),
                         [{"name": "seat0", "capabilities": 2}])
        self.assertEqual(calls.count(["swaymsg", "-r", "-t", "get_seats"]), 3)
        self.assertEqual([c for c in calls if c[0] == "wtype"], [ime.KEYS])

    def test_no_keyboard_exhausts_shared_deadline_without_app_or_typing(self):
        result, calls, stages, files = self.run_driver("no-keyboard")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("seat0 keyboard capability before app spawn", result.stderr)
        names = [c[0] for c in calls]
        self.assertNotIn("rune-fake", names)
        self.assertNotIn("wtype", names)
        self.assertEqual([s["name"] for s in stages if s["stage"] == "process-start"], ["fcitx"])
        failed = next(event for event in stages if event["stage"] == "failure")
        self.assertEqual(failed["elapsed"], ime.STARTUP_SECONDS)
        self.assertEqual(json.loads(files["wayland-ime-seats-before.json"]),
                         [{"name": "seat0", "capabilities": 0}])
        # A failed evidence capture must leave its recorded attempt.
        if "wayland-ime-seats-failure.json" in files:
            self.assertEqual(json.loads(files["wayland-ime-seats-failure.json"]),
                             [{"name": "seat0", "capabilities": 0}])
        self.assert_failure_evidence(stages, files)

    def test_malformed_seat_reply_retains_raw_failure_evidence_without_app(self):
        result, calls, stages, files = self.run_driver("malformed-seats")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("seat0 keyboard capability before app spawn", result.stderr)
        names = [c[0] for c in calls]
        self.assertNotIn("rune-fake", names)
        self.assertNotIn("wtype", names)
        self.assertEqual(files["wayland-ime-seats-before.json"], "not seat JSON\n")
        self.assertEqual(files["wayland-ime-seats-failure.json"], "not seat JSON\n")
        self.assert_failure_evidence(stages, files)


if __name__ == "__main__":
    unittest.main()
