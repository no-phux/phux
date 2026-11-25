"""Qualification driver contracts. The build driver is test_ios_build."""

import json
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_ios_build import build, qualify, valid_report


class QualificationTests(unittest.TestCase):
    def test_native_report_requires_the_control_stability_contract(self):
        report = valid_report()
        self.assertIn("native-control-stability", report["input_contracts"])
        report["input_contracts"].remove("native-control-stability")
        with self.assertRaisesRegex(ValueError, "contract groups did not pass"):
            qualify.validate_report(report)

    def test_native_report_proves_the_requested_transport_not_just_any_connection(self):
        report = valid_report()
        with self.assertRaises(ValueError):
            qualify.validate_report(report, "wss")
        report["transport"] = "paired-wss"
        qualify.validate_report(report, "wss")

    def test_selects_only_runtime_supported_device_not_newer_incompatible_phone(self):
        types = [{"identifier": "17", "name": "iPhone 17", "productFamily": "iPhone"},
                 {"identifier": "duo", "name": "iPhone Duo", "productFamily": "iPhone"}]
        runtimes = [
            {"identifier": "com.apple.iOS-27-1", "version": "27.1", "isAvailable": True,
             "supportedDeviceTypes": [types[1]]},
            {"identifier": "com.apple.iOS-27-0", "version": "27.0", "isAvailable": True,
             "supportedDeviceTypes": [types[0]]},
        ]
        self.assertEqual(qualify.simulator_pair(runtimes, types), ("com.apple.iOS-27-0", "17"))
        self.assertEqual(qualify.simulator_pair(runtimes[:1], types), ("com.apple.iOS-27-1", "duo"))
        runtimes[1]["isAvailable"] = False
        self.assertEqual(qualify.simulator_pair(runtimes, types), ("com.apple.iOS-27-1", "duo"))
        with self.assertRaises(ValueError):
            qualify.simulator_pair([], types)

    def test_marker_not_in_echo_but_printf_outputs_it(self):
        for marker in (qualify.MARKER, qualify.MARKER + "_RESUMED"):
            with self.subTest(marker=marker):
                command = qualify.marker_command(marker)
                self.assertNotIn(marker, command)
                self.assertNotIn("[", command)
                self.assertNotIn("]", command)
                result = subprocess.run(["/bin/sh", "-c", command], check=True,
                                        capture_output=True, text=True, timeout=5)
                self.assertEqual(result.stdout, "[[" + marker + "]]\n")

    def test_report_rejects_missing_false_wrong_type_and_error(self):
        qualify.validate_report(valid_report())
        for field in valid_report():
            report = valid_report()
            del report[field]
            with self.subTest(missing=field), self.assertRaises(ValueError):
                qualify.validate_report(report)
        for field, value in (("frames", True), ("frames", 0), ("schema", True),
                             ("connected", 1), ("input_through_native", False),
                             ("marker_seen", False), ("terminal_id", ""), ("surface_at_scale", False),
                             ("session", "other"), ("error", "connection failed")):
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                qualify.validate_report({**valid_report(), field: value})

    def test_server_environment_is_new_and_never_inherits_production(self):
        with tempfile.TemporaryDirectory() as temporary, \
                patch.dict(os.environ, {"PHUX_SOCKET": "production", "PHUX_WS_TOKENS": "secret"}):
            env = qualify.server_environment(Path(temporary))
            self.assertNotIn("PHUX_SOCKET", env)
            self.assertNotIn("PHUX_WS_TOKENS", env)
            self.assertEqual(env["HOME"], str(Path(temporary) / "home"))
            self.assertNotEqual(env["PHUX_PROFILE"], "default")

    def lane(self, root):
        return qualify.Qualification(root / "Phux Rune.app", root / "phux", root, root)

    def test_cleanup_only_exact_created_udid_and_owned_server(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.udid = "EXACT-NEW-UDID"
            lane.server = Mock()
            lane.server.poll.return_value = None
            lane.simctl = Mock()
            lane.cleanup()
            lane.server.terminate.assert_called_once_with()
            lane.server.wait.assert_called_once_with(timeout=10)
            self.assertEqual(lane.simctl.call_args_list[0].args, ("shutdown", "EXACT-NEW-UDID"))
            self.assertEqual(lane.simctl.call_args_list[1].args, ("delete", "EXACT-NEW-UDID"))

    def test_cleanup_deletes_even_after_shutdown_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.udid = "EXACT-NEW-UDID"
            lane.simctl = Mock(side_effect=[subprocess.TimeoutExpired("shutdown", 60), ""])
            with self.assertRaises(ValueError):
                lane.cleanup()
            self.assertEqual(lane.simctl.call_count, 2)
            self.assertTrue(json.loads((Path(temporary) / "cleanup.json").read_text())["errors"])

    def test_no_simulator_cleanup_without_owned_creation(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.simctl = Mock()
            lane.cleanup()
            lane.simctl.assert_not_called()

    def test_launch_arguments_and_live_process_check(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.udid = "NEW-UDID"
            lane.server = Mock()
            lane.server.poll.return_value = None
            lane.configuration = {"token":"private-never-in-argv"}
            lane.simctl = Mock(side_effect=["", temporary, f"{qualify.BUNDLE_ID}: 123\n"])
            self.assertEqual(lane.launch(Path("/tmp/owned/s")), 123)
            lookup = lane.simctl.call_args_list[1]
            self.assertEqual(lookup.args, ("get_app_container", "NEW-UDID", qualify.BUNDLE_ID, "data"))
            self.assertEqual(lookup.kwargs, {"timeout": qualify.CONTAINER_TIMEOUT,
                                             "attempts": qualify.CONTAINER_ATTEMPTS})
            launch = lane.simctl.call_args_list[2].args
            self.assertIn("--qualification-input", launch)
            self.assertNotIn("--terminate-running-process", launch)
            self.assertNotIn("private-never-in-argv", str(launch))
            self.assertIn("--qualification-configuration", launch)
            secret = Path(temporary) / "Documents/qualification-connection.json"
            self.assertEqual(secret.stat().st_mode & 0o777, 0o600)
            self.assertEqual(launch[launch.index("--qualification-session") + 1], qualify.SESSION)
            # Liveness asks the host for the app's process, never the stall-prone simulator.
            app = ("/Users/runner/Library/Developer/CoreSimulator/Devices/new-udid/data/"
                   "Containers/Bundle/Application/A/Phux Rune.app/PhuxRune\n")
            with patch.object(qualify._host.subprocess, "run") as ps:
                ps.return_value.stdout = app
                lane.require_alive(123)
                self.assertEqual(ps.call_args.args[0], ["/bin/ps", "-o", "comm=", "-p", "123"])
                for other in ("", app.replace("new-udid", "OTHER-UDID"),
                              "/RuntimeRoot/Applications/Preferences.app/Preferences\n"):
                    ps.return_value.stdout = other
                    with self.assertRaisesRegex(ValueError, "PID 123 exited"):
                        lane.require_alive(123)
            self.assertEqual(lane.simctl.call_count, 3)

    def test_simulator_host_strips_all_inherited_phux_and_child_env(self):
        with tempfile.TemporaryDirectory() as temporary, patch.dict(os.environ, {
                "PHUX_SOCKET": "production", "PHUX_SOMETHING_NEW": "secret",
                "SIMCTL_CHILD_PHUX_SOCKET": "production", "DEVELOPER_DIR": "/nix/sdk"}):
            lane = self.lane(Path(temporary))
            self.assertFalse(any(k.startswith(("PHUX_", "SIMCTL_CHILD_")) for k in lane.env))
            self.assertNotIn("DEVELOPER_DIR", lane.env)

    def test_fixture_requires_both_enrolled_identity_and_bearer_auth_even_on_loopback(self):
        with tempfile.TemporaryDirectory() as temporary:
            env = qualify.server_environment(Path(temporary))
            self.assertEqual(env["PHUX_WS_SECURE"], "1")
            self.assertNotIn("PHUX_SOCKET", env)

    def test_external_commands_have_deadlines_and_do_not_use_shell(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            with patch.object(qualify.subprocess, "run") as run:
                lane.simctl("bootstatus", "EXACT", "-b", timeout=240)
            self.assertEqual(run.call_args.args[0],
                             ["/usr/bin/xcrun", "simctl", "bootstatus", "EXACT", "-b"])
            self.assertEqual(run.call_args.kwargs["timeout"], 240)
            self.assertNotIn("shell", run.call_args.kwargs)

    def test_a_stuck_container_lookup_is_retried_then_gives_up(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            stuck = subprocess.TimeoutExpired(["simctl"], qualify.CONTAINER_TIMEOUT)
            lane.run = Mock(side_effect=[stuck, "/data/container\n"])
            self.assertEqual(lane.app_container(), Path("/data/container"))
            self.assertEqual(lane.run.call_args.kwargs["timeout"], qualify.CONTAINER_TIMEOUT)
            lane.run = Mock(side_effect=stuck)
            with self.assertRaises(subprocess.TimeoutExpired):
                lane.app_container()
            self.assertEqual(lane.run.call_count, qualify.CONTAINER_ATTEMPTS)

    def test_report_collection_copies_evidence_and_checks_process_after_dwell(self):
        from test_ios_evidence import paired_evidence

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            container = root / "container"
            documents = container / "Documents"
            documents.mkdir(parents=True)
            tree, report = paired_evidence()
            (documents / "rune-qualification.json").write_text(json.dumps(report))
            (documents / "rune-tree.json").write_text(json.dumps(tree))
            lane = self.lane(root)
            lane.documents = documents
            lane.require_alive = Mock()

            def simctl(*args, **kwargs):
                if args[0] == "launch" and args[-1] == "com.apple.springboard":
                    (documents / "rune-background.json").write_text('{"background_frames":4}')
                if args[0] == "launch" and args[-1] == qualify.BUNDLE_ID:
                    (documents / "rune-lifecycle.json").write_text(json.dumps({"background_frames":4,
                        "resumed_frames":5,"marker_seen":True}))
                if args[0] == "io":
                    Path(args[-1]).write_bytes(b"screenshot")
                return ""

            lane.simctl = Mock(side_effect=simctl)
            with patch.object(qualify.time, "sleep") as sleep:
                self.assertEqual(lane.collect_report(123), report)
            sleep.assert_called_once_with(3)
            self.assertEqual(lane.require_alive.call_count, 3)
            self.assertTrue((root / "rune-lifecycle.json").is_file())
            self.assertTrue((root / "rune-tree.json").is_file())
            self.assertTrue((root / "ios-native.png").is_file())
            self.assertFalse(any(call.args[0] == "get_app_container" for call in lane.simctl.call_args_list))

    def test_failed_qualification_still_captures_failure_and_cleans_owned_resources(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app = root / "App"
            app.mkdir()
            (app / "Info.plist").write_bytes(plistlib.dumps(build.bundle_plist("1.0", True, [])))
            with patch.object(qualify, "Qualification") as constructor:
                lane = constructor.return_value
                lane.launch.side_effect = ValueError("native launch failed")
                with self.assertRaisesRegex(ValueError, "native launch failed"):
                    qualify.qualify(app, root / "phux", root / "out")
            lane.failure_screenshot.assert_called_once_with()
            lane.cleanup.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
