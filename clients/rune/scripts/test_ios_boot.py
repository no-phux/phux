"""Execute the real bootstrap/retry/cleanup policy with inert command results."""
import json
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_ios_build import load

qualify = load("qualify")
OWNED = "01234567-89ab-4cde-8fab-0123456789ab"
PHONE = {"identifier": "phone-17", "name": "iPhone 17", "productFamily": "iPhone"}
RUNTIME = {"identifier": "com.apple.iOS-27-0", "version": "27.0", "isAvailable": True,
           "supportedDeviceTypes": [PHONE]}


class Bootstrap(unittest.TestCase):
    def lane(self, root, boot_error=None, create_error=None, created=OWNED.upper()):
        app = root / "App"
        app.mkdir()
        (app / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": qualify.BUNDLE_ID,
            "CFBundleSupportedPlatforms": ["iPhoneSimulator"],
        }))
        out = root / "out"
        out.mkdir()
        lane = qualify.Qualification(app, root / "phux", out, root)

        def run(command, timeout=30, env=None):
            self.assertEqual(command[:2], ["/usr/bin/xcrun", "simctl"])
            action = command[2]
            if action == "list":
                return json.dumps({"runtimes": [RUNTIME], "devicetypes": [PHONE]})
            if action == "create":
                self.assertEqual(command[-2:], [PHONE["identifier"], RUNTIME["identifier"]])
                if create_error:
                    raise create_error
                return created
            if action == "bootstatus":
                self.assertEqual(command[3:], [OWNED, "-b"])
                self.assertEqual(timeout, 240)
                if boot_error:
                    raise boot_error
                return "Finished"
            if action in ("shutdown", "delete"):
                self.assertEqual(command[3:], [OWNED])
                self.assertEqual(timeout, 60)
                return ""
            self.fail(f"unexpected action {action}; bootstatus -b owns the single boot operation")

        lane.run = Mock(side_effect=run)
        lane.settle = Mock()
        return lane

    def test_boot_and_readiness_are_one_owned_operation_before_settling(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))

            def settle():
                self.assertEqual(lane.run.call_count, 4)
                self.assertEqual(lane.run.call_args.args[0][2:], ["bootstatus", OWNED, "-b"])

            lane.settle.side_effect = settle
            lane.create_simulator()
            self.assertEqual(lane.udid, OWNED)
            self.assertEqual([call.args[0][2] for call in lane.run.call_args_list],
                             ["list", "list", "create", "bootstatus"])
            lane.settle.assert_called_once_with()
            self.assertEqual(json.loads((lane.out / "simulator.json").read_text()), {
                "udid": OWNED, "runtime": RUNTIME["identifier"], "device_type": PHONE["identifier"],
            })

    def assert_outer_failure(self, lane, error, owned):
        lane.start_server = Mock(return_value="/tmp/owned-only/s")
        lane.server = Mock()
        lane.server.poll.return_value = None
        lane.failure_screenshot = Mock()
        lane.launch = Mock()
        lane.collect_report = Mock()
        lane.resume_identity = Mock()
        with patch.object(qualify, "Qualification", return_value=lane):
            with self.assertRaises(type(error)) as raised:
                qualify.qualify(lane.app, lane.phux, lane.out)
        if not isinstance(error, ValueError):
            self.assertIs(raised.exception, error)
        lane.failure_screenshot.assert_called_once_with()
        lane.server.terminate.assert_called_once_with()
        lane.server.wait.assert_called_once_with(timeout=10)
        lane.settle.assert_not_called()
        lane.launch.assert_not_called()
        lane.collect_report.assert_not_called()
        lane.resume_identity.assert_not_called()
        actions = [call.args[0][2] for call in lane.run.call_args_list]
        if owned:
            self.assertEqual(actions, ["list", "list", "create", "bootstatus", "shutdown", "delete"])
        else:
            self.assertEqual(actions, ["list", "list", "create"])
            self.assertIsNone(lane.udid)
        self.assertFalse((lane.out / "evidence.json").exists())
        self.assertEqual(json.loads((lane.out / "cleanup.json").read_text()), {"errors": []})

    def test_boot_timeout_and_nonzero_exit_propagate_once_without_launch_or_retry(self):
        for error in (subprocess.TimeoutExpired("bootstatus", 240),
                      subprocess.CalledProcessError(1, "bootstatus")):
            with self.subTest(error=type(error)), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary), boot_error=error)
                self.assert_outer_failure(lane, error, owned=True)

    def test_failed_creation_cannot_adopt_boot_or_delete_an_unowned_device(self):
        for error in (subprocess.TimeoutExpired("create", 30),
                      subprocess.CalledProcessError(1, "create"), ValueError("invalid UUID")):
            with self.subTest(error=type(error)), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary),
                                 create_error=None if isinstance(error, ValueError) else error,
                                 created="not-an-owned-uuid")
                self.assert_outer_failure(lane, error, owned=False)


if __name__ == "__main__":
    unittest.main()
