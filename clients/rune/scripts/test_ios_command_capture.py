"""Bounded observation lifecycle tests; host tools are mocks or inert children."""
import json
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from test_ios_command_diagnostics import diagnose, identity, stack_report


class CommandCaptureTests(unittest.TestCase):
    def lane(self, root):
        return SimpleNamespace(out=root, sequence=0, env={"PATH": "/usr/bin:/bin"},
                               server=None, diagnostic_pid=None, udid="OWNED", phux=root / "phux")

    def test_helper_identity_error_kills_and_reaps_only_retained_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            observation = diagnose.Observation(self.lane(root), Mock(pid=123), root / "command-001",
                                              identity(), {})
            spawned = []
            popen = subprocess.Popen

            def spawn(*args, **kwargs):
                child = popen(*args, **kwargs)
                spawned.append(child)
                return child

            with patch.object(diagnose.subprocess, "Popen", side_effect=spawn), \
                    patch.object(diagnose, "process_identity", side_effect=OSError("inert identity error")):
                record = observation.helper("inert", [sys.executable, "-c", "import time; time.sleep(5)"])
            self.assertEqual(record["error"], "OSError")
            self.assertIsNotNone(spawned[0].returncode)
            self.assertFalse(observation.helpers)

    def run_inert_capture(self, root, delay, sample_timeout=1, capture_timeout=2):
        """Real helper lifecycle; command, identities and all host tools are inert."""
        lane = self.lane(root)
        command = Mock(pid=123)
        command.poll.return_value = None
        started = threading.Event()
        command.wait.side_effect = lambda **kwargs: 0 if started.wait(1) else self.fail("no sampler")
        popen = subprocess.Popen
        spawned = []

        def spawn(args, **kwargs):
            if args[0] == "/inert/command":
                return command
            self.assertEqual(args[:4], ["/usr/bin/sample", "123", "1", "20"])
            self.assertIn("-mayDie", args)
            destination = args[args.index("-file") + 1]
            script = ("import time; from pathlib import Path; "
                      f"time.sleep({delay}); Path({destination!r}).write_text({stack_report()!r})")
            child = popen([sys.executable, "-c", script], **kwargs)
            spawned.append(child)
            started.set()
            return child

        # Scale the cleanup reserve with the compressed capture clock.
        with patch.object(diagnose.subprocess, "Popen", side_effect=spawn), \
                patch.object(diagnose, "process_identity", side_effect=lambda pid: identity(pid)), \
                patch.object(diagnose.Observation, "resources", return_value=[]), \
                patch.object(diagnose, "STALL_SECONDS", 0.01), \
                patch.object(diagnose, "SAMPLE_SECONDS", sample_timeout), \
                patch.object(diagnose, "CLEANUP_SECONDS", min(0.25, capture_timeout / 2)), \
                patch.object(diagnose, "CAPTURE_SECONDS", capture_timeout):
            diagnose.diagnostic_run(lane, ["/inert/command"], timeout=3)
        self.assertEqual(len(spawned), 1)
        self.assertIsNotNone(spawned[0].returncode)
        command.kill.assert_not_called()
        command.wait.assert_called_once()
        self.assertLessEqual(command.wait.call_args.kwargs["timeout"], 3)
        timing = json.loads((root / "command-001.timing.json").read_text())
        capture = json.loads((root / "command-001-command-capture.json").read_text())
        return timing, capture, spawned[0]

    def test_slow_real_helper_completes_after_original_command_exit(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            timing, capture, helper = self.run_inert_capture(root, delay=0.2)
            self.assertTrue(timing["succeeded"])
            self.assertTrue(capture["capture_complete"])
            self.assertEqual(helper.returncode, 0)
            self.assertGreater(timing["elapsed_seconds"] - timing["command_elapsed_seconds"], 0.1)
            self.assertLess(timing["finished_utc"], capture["finished_utc"])
            self.assertEqual(capture["helper"]["target_identity_at_start"], identity())
            self.assertTrue(diagnose.capture_summary(root)["capture_complete"])

    def test_real_helper_timeout_is_bounded_and_reaped_without_failing_sequence(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            timing, capture, helper = self.run_inert_capture(root, delay=5, sample_timeout=0.05)
            self.assertTrue(timing["succeeded"])
            self.assertFalse(capture["capture_complete"])
            self.assertTrue(capture["helper"]["timed_out"])
            self.assertLess(helper.returncode, 0)
            self.assertLess(timing["elapsed_seconds"], 1)
            self.assertFalse(diagnose.capture_summary(root)["capture_complete"])

    def test_global_capture_budget_cancels_and_reaps_real_helper(self):
        with tempfile.TemporaryDirectory() as temporary:
            timing, capture, helper = self.run_inert_capture(
                Path(temporary), delay=5, sample_timeout=5, capture_timeout=0.5)
            self.assertTrue(timing["succeeded"])
            self.assertFalse(capture["capture_complete"])
            self.assertTrue(capture["helper"]["cancelled"], capture)
            self.assertLess(helper.returncode, 0)
            self.assertLess(timing["elapsed_seconds"], 1)

    def test_resources_and_all_owned_samplers_start_without_serial_waits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            targets = {role: identity(pid) for role, pid in (("command", 123), ("app", 124), ("server", 125))}
            process = Mock(pid=123)
            process.poll.return_value = None
            observation = diagnose.Observation(self.lane(root), process, root / "command-001",
                                              identity(), targets)
            # Eight workers must all reach this barrier before any completes.
            barrier = threading.Barrier(8, timeout=1)
            calls = []

            def capture(label, *args):
                calls.append(label)
                barrier.wait()

            with patch.object(diagnose, "process_identity", return_value=identity()), \
                    patch.object(diagnose, "STALL_SECONDS", 0), \
                    patch.object(observation, "helper", side_effect=capture), \
                    patch.object(observation, "sample", side_effect=capture):
                observation.observe()
            self.assertEqual(set(calls), {"host", "ps", "command", "app", "server",
                                          "top", "vm", "service-log"})
            self.assertFalse(barrier.broken)
            self.assertFalse((root / "command-001.observation-error.json").exists())

    def test_successful_sampler_with_no_stack_is_incomplete(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            observation = diagnose.Observation(self.lane(root), Mock(pid=123), root / "command-001",
                                              identity(), {"command": identity()})
            diagnose.write_json(root / "command-001.observation.json",
                                {"owned_targets": {"command": identity()}})
            helper = {"returncode": 0, "target_identity_at_start": identity(), "cleanup_complete": True}
            with patch.object(diagnose, "process_identity", return_value=identity()), \
                    patch.object(observation, "helper", return_value=helper):
                observation.sample("command", identity())
            capture = json.loads((root / "command-001-command-capture.json").read_text())
            self.assertEqual(capture["helper"]["returncode"], 0)
            self.assertEqual(capture["report_bytes"], 0)
            self.assertFalse(capture["capture_complete"])
            self.assertEqual(diagnose.capture_summary(root)["incomplete_samples"], 1)
            self.assertFalse(diagnose.capture_summary(root)["capture_complete"])

    def test_unreapable_helper_timeout_is_explicit_and_bounded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            observation = diagnose.Observation(self.lane(root), Mock(pid=123), root / "command-001",
                                              identity(), {})
            helper = Mock(pid=456, returncode=None)
            helper.poll.return_value = None
            helper.wait.side_effect = [subprocess.TimeoutExpired("helper", 5),
                                       subprocess.TimeoutExpired("helper-cleanup", 1)]
            with patch.object(diagnose.subprocess, "Popen", return_value=helper), \
                    patch.object(diagnose, "process_identity", return_value=identity(456)):
                record = observation.helper("inert", ["/inert/helper"])
            self.assertFalse(record["cleanup_complete"])
            self.assertEqual(record["cleanup_error"], "TimeoutExpired")
            self.assertTrue(observation.helpers)
            for call in helper.wait.call_args_list:
                self.assertIsNotNone(call.kwargs["timeout"])
                self.assertGreaterEqual(call.kwargs["timeout"], 0)
            observation.thread.start()
            cleanup = observation.stop()
            self.assertFalse(cleanup["cleanup_complete"])
            self.assertEqual(cleanup["unreaped_helpers"][0]["pid"], 456)
            self.assertFalse(cleanup["unreaped_helpers"][0]["reaped"])

    def run_stuck_sequence(self, root, failure):
        """A fake post-kill wait ignores its timeout until explicit teardown."""
        released = threading.Event()
        blocked_pending = threading.Event()
        sibling_pending = threading.Event()
        sibling_killed = threading.Event()
        observations = []
        command = Mock(pid=123, returncode=None)
        command.poll.return_value = None
        blocked = Mock(pid=456, returncode=None)
        blocked.poll.return_value = None
        sibling = Mock(pid=457, returncode=None)
        sibling.poll.side_effect = lambda: sibling.returncode

        def blocked_wait(timeout):
            self.assertGreaterEqual(timeout, 0)
            if blocked.wait.call_count == 1:
                raise subprocess.TimeoutExpired("inert helper", timeout)
            blocked_pending.set()
            if not released.wait(2):
                raise AssertionError("test did not release its fake worker")
            blocked.returncode = -9
            return -9

        def sibling_wait(timeout):
            self.assertGreaterEqual(timeout, 0)
            sibling_pending.set()
            if not sibling_killed.wait(2):
                raise AssertionError("sibling cancellation was blocked")
            sibling.returncode = -9
            return -9

        def command_wait(timeout):
            self.assertLessEqual(timeout, 2)
            if command.returncode is None:
                self.assertTrue(blocked_pending.wait(1))
                self.assertTrue(sibling_pending.wait(1))
                command.returncode = -9 if failure is not None else 0
                if failure is not None:
                    raise failure
            return command.returncode

        blocked.wait.side_effect = blocked_wait
        sibling.wait.side_effect = sibling_wait
        sibling.kill.side_effect = sibling_killed.set
        command.wait.side_effect = command_wait
        children = {"/inert/command": command, "/inert/blocked": blocked, "/inert/sibling": sibling}
        observation_class = diagnose.Observation

        def observe(*args):
            observation = observation_class(*args)
            observations.append(observation)
            return observation

        def qualify(app, phux, directory, transport):
            directory.mkdir()
            if transport == "quic":
                diagnose.diagnostic_run(self.lane(directory), ["/inert/command"], timeout=2)

        app = root / "app"
        app.mkdir()
        (app / "Info.plist").write_bytes(plistlib.dumps({"CFBundleExecutable": "PhuxRune"}))
        module = SimpleNamespace(Qualification=object, qualify=qualify)
        try:
            with patch.object(diagnose, "load_qualifier", return_value=module), \
                    patch.object(diagnose, "Observation", side_effect=observe), \
                    patch.object(observation_class, "resources", return_value=[
                        ("blocked", ["/inert/blocked"]), ("sibling", ["/inert/sibling"])]), \
                    patch.object(diagnose, "owned_targets", return_value={}), \
                    patch.object(diagnose, "process_identity", side_effect=lambda pid: identity(pid)), \
                    patch.object(diagnose.subprocess, "Popen", side_effect=lambda args, **kwargs: children[args[0]]), \
                    patch.object(diagnose, "STALL_SECONDS", 0.001), \
                    patch.object(diagnose, "CAPTURE_SECONDS", 0.2), \
                    patch.object(diagnose, "CLEANUP_SECONDS", 0.05):
                started = time.monotonic()
                if failure is None:
                    diagnose.run_sequence(app, root / "phux", root / "out")
                else:
                    with self.assertRaises(type(failure)) as raised:
                        diagnose.run_sequence(app, root / "phux", root / "out")
                    self.assertIs(raised.exception, failure)
                self.assertLess(time.monotonic() - started, 0.8)
            self.assertFalse(released.is_set())
            sibling.kill.assert_called()
            blocked.kill.assert_called()
            self.assertEqual(sibling.returncode, -9)
            timing = json.loads((root / "out/quic/command-001.timing.json").read_text())
            cleanup = json.loads((root / "out/quic/command-001.cleanup.json").read_text())
            status = json.loads((root / "out/quic-diagnostic-status.json").read_text())
            self.assertEqual(timing["succeeded"], failure is None)
            self.assertEqual(status["sequence_completed"], failure is None)
            self.assertFalse(timing["diagnostic_cleanup"]["cleanup_complete"])
            self.assertFalse(cleanup["cleanup_complete"])
            self.assertTrue(cleanup["external_blocker"])
            retained = {record["pid"]: record for record in cleanup["unreaped_helpers"]}
            self.assertEqual(retained[456]["identity"], identity(456))
            self.assertFalse(retained[456]["reaped"])
            self.assertIn("blocked", cleanup["unfinished_workers"])
            self.assertFalse(status["capture"]["cleanup_complete"])
            self.assertFalse(status["capture"]["capture_complete"])
            self.assertTrue(status["capture"]["incomplete_cleanup"])
        finally:
            released.set()
            for observation in observations:
                for _, worker in observation.workers:
                    worker.join(timeout=1)
                    self.assertFalse(worker.is_alive())
                observation.thread.join(timeout=1)
                self.assertFalse(observation.thread.is_alive())

    def test_stuck_worker_preserves_original_exception_and_writes_incomplete_status(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.run_stuck_sequence(Path(temporary), subprocess.TimeoutExpired("original command", 2))

    def test_stuck_worker_preserves_success_and_writes_incomplete_status(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.run_stuck_sequence(Path(temporary), None)

    def test_original_command_cleanup_timeout_cannot_replace_original_failure(self):
        original = subprocess.TimeoutExpired("original command", 2)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            command = Mock(pid=123)
            command.poll.return_value = None
            command.wait.side_effect = [original, subprocess.TimeoutExpired("cleanup", 1)]
            observer = Mock()
            observer.stop.return_value = {"cleanup_complete": True}
            with patch.object(diagnose.subprocess, "Popen", return_value=command), \
                    patch.object(diagnose, "process_identity", return_value=identity()), \
                    patch.object(diagnose, "Observation", return_value=observer):
                with self.assertRaises(subprocess.TimeoutExpired) as raised:
                    diagnose.diagnostic_run(self.lane(root), ["/inert/command"], timeout=2)
            self.assertIs(raised.exception, original)
            self.assertEqual(command.wait.call_args.kwargs["timeout"], diagnose.CLEANUP_SECONDS)
            timing = json.loads((root / "command-001.timing.json").read_text())
            self.assertEqual(timing["command_cleanup_error"], "TimeoutExpired")
            self.assertFalse(timing["succeeded"])


if __name__ == "__main__":
    unittest.main()
