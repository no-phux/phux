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


if __name__ == "__main__":
    unittest.main()
