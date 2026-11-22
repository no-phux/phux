"""Cold simulator setup and machine-readable command evidence contracts."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

path = Path(__file__).resolve().parents[1] / "ios/qualify.py"
spec = importlib.util.spec_from_file_location("ios_container_qualify", path)
qualify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualify)


class ContainerTests(unittest.TestCase):
    def lane(self, root):
        return qualify.Qualification(root / "Phux Rune.app", root / "phux", root, root)

    def test_container_setup_rejects_empty_relative_and_missing_paths_without_launching(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.udid = "NEW-UDID"
            for container in ["", "relative/path", str(Path(temporary) / "absent")]:
                lane.simctl = Mock(side_effect=["", container])
                with self.subTest(container=container), self.assertRaisesRegex(ValueError, "container"):
                    lane.launch(Path("/tmp/owned/s"))
                self.assertEqual(lane.simctl.call_count, 2)
                self.assertIsNone(lane.documents)

    def test_collect_report_requires_the_prelaunch_container(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.simctl = Mock()
            with self.assertRaisesRegex(ValueError, "before launch"):
                lane.collect_report(123)
            lane.simctl.assert_not_called()

    def test_command_stdout_is_not_polluted_by_service_diagnostics(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lane = self.lane(root)
            def command(*args, stdout, stderr, **kwargs):
                stdout.write("/an/app/container\n")
                stderr.write("CoreSimulator service warning\n")
            with patch.object(qualify.subprocess, "run", side_effect=command):
                self.assertEqual(lane.simctl("get_app_container"), "/an/app/container\n")
            self.assertEqual((root / "command-001.stderr.log").read_text(), "CoreSimulator service warning\n")

    def test_command_timing_records_success_and_failure_without_credential_contents(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lane = self.lane(root)
            with patch.object(qualify.subprocess, "run"), patch.object(qualify.time, "monotonic", side_effect=[10, 12]):
                lane.simctl("get_app_container", "NEW", qualify.BUNDLE_ID, "data", timeout=120)
            self.assertEqual(json.loads((root / "command-001.timing.json").read_text()), {
                "elapsed_seconds": 2, "deadline_seconds": 120, "succeeded": True})
            with patch.object(qualify.subprocess, "run", side_effect=subprocess.TimeoutExpired("simctl", 120)), \
                    patch.object(qualify.time, "monotonic", side_effect=[20, 140]), self.assertRaises(subprocess.TimeoutExpired):
                lane.simctl("get_app_container", "NEW", qualify.BUNDLE_ID, "data", timeout=120)
            self.assertEqual(json.loads((root / "command-002.timing.json").read_text()), {
                "elapsed_seconds": 120, "deadline_seconds": 120, "succeeded": False})

    def settle_with(self, root, durations):
        """Run settle() on a fake clock; each query takes the next duration
        (None: it times out) and each pause between queries one second."""
        clock = [0.0]
        def query(*args, **kwargs):
            took = durations.pop(0)
            clock[0] += 30 if took is None else took
            if took is None:
                raise subprocess.TimeoutExpired("simctl", 30)
            return ""
        def pause(seconds):
            clock[0] += seconds
        lane = self.lane(root)
        lane.udid = "NEW"
        lane.simctl = Mock(side_effect=query)
        with patch.object(qualify._settle.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(qualify._settle.time, "sleep", side_effect=pause):
            settled = lane.settle()
        return settled, lane, json.loads((root / "settle.json").read_text())

    def test_settle_waits_for_consecutive_prompt_answers_after_a_slow_boot(self):
        with tempfile.TemporaryDirectory() as temporary:
            settled, lane, record = self.settle_with(
                Path(temporary), [24.0, None, 0.5, 8.0, 0.25, 0.75, 0.5, 99.0])
            self.assertTrue(settled)
            self.assertEqual(record["query_seconds"], [24.0, None, 0.5, 8.0, 0.25, 0.75, 0.5])
            self.assertEqual(lane.simctl.call_count, 7)
            lane.simctl.assert_called_with("spawn", "NEW", "launchctl", "list")

    def test_settle_is_bounded_and_not_fatal_when_the_simulator_stays_slow(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch("sys.stderr"):
                settled, lane, record = self.settle_with(Path(temporary), [20.0] * 100)
            self.assertFalse(settled)
            self.assertLessEqual(record["elapsed_seconds"], qualify._settle.SETTLE_BUDGET + 21)
            self.assertEqual(lane.simctl.call_count, len(record["query_seconds"]))

    def test_only_a_retried_query_survives_a_stuck_simctl_call(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            stuck = subprocess.TimeoutExpired("simctl", 30)
            lane.run = Mock(side_effect=[stuck, "listing"])
            self.assertEqual(lane.simctl("spawn", "NEW", "launchctl", "list", attempts=3), "listing")
            lane.run = Mock(side_effect=[stuck, "pid"])
            with self.assertRaises(subprocess.TimeoutExpired):
                lane.simctl("launch", "NEW", qualify.BUNDLE_ID)
            lane.run.assert_called_once()


if __name__ == "__main__":
    unittest.main()
