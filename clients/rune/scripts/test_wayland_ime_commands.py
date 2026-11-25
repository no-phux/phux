"""Command-boundary and clock tests for the Wayland IME diagnostic."""

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import wayland_ime_fixture as fixture
from wayland_ime_fixture import (
    CommandObservationFixture, DRIVER, FixtureClock, FixtureProcesses,
)

SPEC = importlib.util.spec_from_file_location("wayland_ime", DRIVER)
ime = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ime)

class CommandObservationTests(CommandObservationFixture, unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.artifact = "wayland-ime-seats-before.json"
        with patch.object(ime.time, "monotonic", return_value=100):
            self.driver = ime.Diagnostic(directory.name, "fake-app", "fake-phux", "socket", "session")
        self.events = []
        event_patch = patch.object(self.driver, "event", side_effect=self.record_event)
        event_patch.start()
        self.addCleanup(event_patch.stop)
        self.previous_out = b'[{"name":"seat0","capabilities":0}]\n'
        self.previous_err = b"previous stderr\xff\n"
        self.next_out = b'[{"name":"seat0","capabilities":2}]\n'
        self.next_err = b"new stderr\n"
        self.returncode = 0
        self.failure = None

    def test_expired_deadline_preserves_previous_pair_without_execution(self):
        self.seed_previous()
        with patch.object(ime.time, "monotonic", return_value=109):
            with patch.object(ime.subprocess, "run") as run:
                with self.assertRaisesRegex(RuntimeError, "deadline expired"):
                    self.driver.command(["fixture-only"], self.artifact)
                run.assert_not_called()
        self.assert_previous()
        self.assertIn({"stage": "command-not-executed", "artifact": self.artifact,
                       "status": "deadline-expired"}, self.events)

    def test_expired_deadline_without_observation_creates_no_canonical_files(self):
        with patch.object(ime.time, "monotonic", return_value=109):
            with patch.object(ime.subprocess, "run") as run:
                with self.assertRaisesRegex(RuntimeError, "deadline expired"):
                    self.driver.command(["fixture-only"], self.artifact)
                run.assert_not_called()
        self.assertEqual(list(self.root.iterdir()), [])

    def test_deadline_expiring_after_entry_does_not_execute_or_replace(self):
        self.seed_previous()
        with patch.object(ime.time, "monotonic", side_effect=[108.999, 109]):
            with patch.object(ime.subprocess, "run", side_effect=self.complete_command) as run:
                with self.assertRaisesRegex(RuntimeError, "deadline expired"):
                    self.driver.command(["fixture-only"], self.artifact)
                run.assert_not_called()
        self.assert_previous()
        self.assertIn({"stage": "command-not-executed", "artifact": self.artifact,
                       "status": "deadline-expired"}, self.events)

    def test_tiny_remaining_timeout_preserves_pair_and_partial_output(self):
        self.seed_previous()
        self.failure = subprocess.TimeoutExpired(["fixture-only"], 0.001)
        with patch.object(ime.time, "monotonic", return_value=108.999):
            with patch.object(ime.subprocess, "run", side_effect=self.complete_command) as run:
                with self.assertRaises(subprocess.TimeoutExpired):
                    self.driver.command(["fixture-only"], self.artifact)
                self.assertAlmostEqual(run.call_args.kwargs["timeout"], 0.001)
        self.assert_previous()
        self.assert_failed_attempt("TimeoutExpired")

    def test_nonzero_exit_retains_previous_pair_and_failed_attempt(self):
        self.seed_previous()
        self.returncode = 7
        with patch.object(ime.time, "monotonic", return_value=108):
            with patch.object(ime.subprocess, "run", side_effect=self.complete_command):
                with self.assertRaisesRegex(RuntimeError, "command failed \\(7\\)"):
                    self.driver.command(["fixture-only"], self.artifact)
        self.assert_previous()
        self.assert_failed_attempt("RuntimeError")
        exits = [e for e in self.events if e["stage"] == "command-exit"]
        self.assertEqual(exits[-1]["returncode"], 7)

    def test_fresh_failure_records_raw_output_without_faking_canonical_reply(self):
        self.returncode = 6
        self.next_out = b"partial non-JSON reply\n"
        with patch.object(ime.time, "monotonic", return_value=108):
            with patch.object(ime.subprocess, "run", side_effect=self.complete_command):
                with self.assertRaisesRegex(RuntimeError, "command failed \\(6\\)"):
                    self.driver.command(["fixture-only"], self.artifact)
        self.assertFalse((self.root / self.artifact).exists())
        self.assertFalse((self.root / (self.artifact + ".stderr.log")).exists())
        self.assert_failed_attempt("RuntimeError")

    def test_completed_success_replaces_both_only_after_process_completion(self):
        self.seed_previous()

        def complete_after_checks(args, **options):
            self.assert_previous()
            result = self.complete_command(args, **options)
            self.assert_previous()
            return result

        with patch.object(ime.time, "monotonic", return_value=108):
            with patch.object(ime.subprocess, "run", side_effect=complete_after_checks):
                reply = self.driver.command(["fixture-only"], self.artifact)
        self.assertEqual(reply, self.next_out.decode())
        self.assertEqual((self.root / self.artifact).read_bytes(), self.next_out)
        self.assertEqual((self.root / (self.artifact + ".stderr.log")).read_bytes(), self.next_err)
        self.assertEqual({p.name for p in self.root.iterdir()},
                         {self.artifact, self.artifact + ".stderr.log"})

    def test_completed_malformed_reply_is_published_for_parser_failure(self):
        self.seed_previous()
        self.next_out = b"not seat JSON\n"
        with patch.object(ime.time, "monotonic", return_value=108):
            with patch.object(ime.subprocess, "run", side_effect=self.complete_command):
                reply = self.driver.command(["fixture-only"], self.artifact)
        self.assertEqual((self.root / self.artifact).read_bytes(), self.next_out)
        self.assertEqual((self.root / (self.artifact + ".stderr.log")).read_bytes(), self.next_err)
        with self.assertRaises(ValueError):
            ime.seat_keyboard_ready(reply)

    def test_spawn_error_is_recorded_without_erasing_observation(self):
        self.seed_previous()
        self.next_out = b""
        self.next_err = b""
        self.failure = FileNotFoundError("fixture executable absent")
        with patch.object(ime.time, "monotonic", return_value=108):
            with patch.object(ime.subprocess, "run", side_effect=self.complete_command):
                with self.assertRaises(FileNotFoundError):
                    self.driver.command(["fixture-only"], self.artifact)
        self.assert_previous()
        self.assert_failed_attempt("FileNotFoundError")

    @patch.dict(os.environ, {"MODE": "success"})
    def test_ordinary_command_work_counts_against_shared_deadline(self):
        clock = FixtureClock()
        clock.now = 108.75
        with patch.object(ime, "time", clock), patch.object(fixture.ime, "time", clock):
            with patch.object(ime, "subprocess", FixtureProcesses()):
                with patch.object(subprocess, "run", side_effect=self.complete_command) as run:
                    with self.assertRaisesRegex(RuntimeError, "timed out waiting for ready"):
                        self.driver.wait_for(
                            "ready", lambda: bool(self.driver.command(["fixture-only"], self.artifact)))
        self.assertEqual(clock.now, 109)
        self.assertEqual(self.driver.deadline, 109)
        run.assert_called_once()
        self.assertEqual((self.root / self.artifact).read_bytes(), self.next_out)
        self.assertFalse(any(e["stage"] == "predicate-ready" for e in self.events))
        exits = [e for e in self.events if e["stage"] == "command-exit"]
        self.assertEqual(exits[-1]["timeout"], 0.25)

    @patch.dict(os.environ, {"MODE": "success"})
    def test_ordinary_command_respects_supplied_and_remaining_budgets(self):
        self.seed_previous()
        for now, cap in ((108, 0.125), (108.875, 20)):
            with self.subTest(now=now, cap=cap):
                clock = FixtureClock()
                clock.now = now
                with patch.object(ime, "time", clock), patch.object(fixture.ime, "time", clock):
                    with patch.object(ime, "subprocess", FixtureProcesses()):
                        with patch.object(subprocess, "run", side_effect=self.complete_command) as run:
                            with self.assertRaises(subprocess.TimeoutExpired) as raised:
                                self.driver.command(["fixture-only"], self.artifact, timeout=cap)
                self.assertEqual(raised.exception.timeout, 0.125)
                self.assertEqual(clock.now, now + 0.125)
                run.assert_not_called()
                self.assert_previous()
                self.assertEqual(
                    (self.root / (self.artifact + ".failed.stdout.log")).read_bytes(), b"")
                self.assertEqual(self.events[-1]["status"], "TimeoutExpired")


class FixtureClockTests(unittest.TestCase):
    def test_sleep_advances_only_by_requested_delay(self):
        clock = FixtureClock()
        with patch.object(fixture.time, "sleep") as sleep:
            clock.sleep(0)
            self.assertEqual(clock.monotonic(), 0)
            clock.sleep(0.125)
            self.assertEqual(clock.monotonic(), 0.125)
            clock.sleep(0.375)
            self.assertEqual(clock.monotonic(), 0.5)
            self.assertEqual(sleep.call_count, 3)


if __name__ == "__main__":
    unittest.main()
