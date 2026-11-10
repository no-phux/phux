"""Focused driver contracts; real Swift/Rust/simulator execution is a CI gate."""

import importlib.util
import fnmatch
import json
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch


def load(name):
    path = Path(__file__).resolve().parents[1] / "ios" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(f"ios_{name}", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


build = load("build")
qualify = load("qualify")


def valid_report():
    return {"schema": 1, "session": qualify.SESSION, "frames": 3,
            "terminal_id": "123", "connected": True, "input_through_native": True,
            "marker_seen": True, "transport":"paired-quic", "input_contracts":list(qualify.INPUT_CONTRACTS),
            "onboarding_import": True, "onboarding_confirmed": True, "resumed_identity": False,
            "surface_at_scale": True,
            "key_id": "11111111-1111-4111-8111-111111111111"}


class BuildTests(unittest.TestCase):
    def test_native_evidence_is_not_matched_by_desktop_release_artifact_glob(self):
        workflow = (build.REPO / ".github/workflows/rune-build.yml").read_text()
        self.assertIn("name: qualification-ios-native", workflow)
        self.assertFalse(fnmatch.fnmatch("qualification-ios-native", "rune-*"))
        self.assertIn("Test native tailnet trust, SSH and cancellation", workflow)
        self.assertIn("--sdk macosx swift test", workflow)
        self.assertIn("--force-resolved-versions", workflow)

    def test_native_flags_keep_order_quotes_and_duplicates(self):
        self.assertEqual(build.native_static_flags(
            'noise\nnote: native-static-libs: -framework Security -lSystem '
            '"-L/a path" -lSystem\n'),
            ["-framework", "Security", "-lSystem", "-L/a path", "-lSystem"])

    def test_missing_empty_and_ambiguous_flags_fail_closed(self):
        for output in ("", "note: native-static-libs: ",
                       "native-static-libs: -la\nnative-static-libs: -lb"):
            with self.subTest(output=output), self.assertRaises(ValueError):
                build.native_static_flags(output)

    def test_plist_deployment_and_bundle_contract(self):
        for simulator, platform in ((True, "iPhoneSimulator"), (False, "iPhoneOS")):
            info = build.bundle_plist("1.2.3-beta", simulator, ["Font.ttf"])
            self.assertEqual(info["CFBundleIdentifier"], qualify.BUNDLE_ID)
            self.assertEqual(info["MinimumOSVersion"], "26.0")
            self.assertEqual(info["CFBundleVersion"], "1.2.3")
            self.assertEqual(info["CFBundleSupportedPlatforms"], [platform])
            self.assertEqual(info["CFBundleURLTypes"][0]["CFBundleURLSchemes"], ["phux-rune"])
            self.assertIn("NSCameraUsageDescription", info)
            self.assertIn("NSLocalNetworkUsageDescription", info)
            self.assertEqual(info["UILaunchScreen"], {"UIColorName": "LaunchGround"})
            self.assertEqual(plistlib.loads(plistlib.dumps(info)), info)
        icons = {"CFBundleIcons": {"CFBundlePrimaryIcon": {"CFBundleIconName": "AppIcon"}}}
        self.assertEqual(build.bundle_plist("1.0.0", False, [], icons)["CFBundleIcons"], icons["CFBundleIcons"])

    def test_build_commands_real_staticlib_and_swift_link(self):
        with tempfile.TemporaryDirectory(prefix="rune path ") as temporary:
            root = Path(temporary)
            ios, rune, out = root / "ios", root / "rune", root / "out"
            (ios / "Sources").mkdir(parents=True)
            (ios / "Sources/Host.swift").touch()
            (ios / "Bridge.h").touch()
            rune.mkdir()
            (rune / "Cargo.toml").write_text('[package]\nversion = "1.2.3"\n')
            commands = []

            def fake_run(command, env, output, name, timeout=120, stdout_only=False):
                commands.append([str(arg) for arg in command])
                if name == "rust":
                    target = command[command.index("--target") + 1]
                    library = output / "cargo" / target / "debug/libphux_rune_ios.a"
                    library.parent.mkdir(parents=True)
                    library.touch()
                    return "note: native-static-libs: -framework Security -lSystem -lSystem\n"
                return {"sdk": "/an sdk", "revision": "abc123"}.get(name, "")

            with patch.multiple(build, IOS=ios, RUNE=rune), \
                    patch.object(build, "apple_environment", return_value={}), \
                    patch.object(build, "copy_resources", return_value=["Font.ttf"]), \
                    patch.object(build, "compile_assets", return_value={}), \
                    patch.object(build, "prepare_tailnet", return_value={
                        "swift_flags": ["-lRuneTailnet"], "frameworks": [], "resources": [], "notices": "SDK notices"}), \
                    patch.object(build, "run", side_effect=fake_run):
                app = build.build(out, "simulator", False)
            rust = next(c for c in commands if c[:2] == ["cargo", "rustc"])
            self.assertIn("--locked", rust)
            self.assertEqual(rust[-3:], ["--", "--print", "native-static-libs"])
            swift = next(c for c in commands if "swiftc" in c)
            self.assertIn("arm64-apple-ios26.0-simulator", swift)
            self.assertIn("-strict-concurrency=complete", swift)
            self.assertIn("-warnings-as-errors", swift)
            self.assertEqual(swift[swift.index("-swift-version") + 1], "6")
            self.assertIn(str(out / "module-cache"), swift)
            self.assertIn("-lRuneTailnet", swift)
            link = swift[swift.index("-Xlinker"):]
            self.assertEqual(link[:8], ["-Xlinker", "-framework", "-Xlinker", "Security",
                                       "-Xlinker", "-lSystem", "-Xlinker", "-lSystem"])
            self.assertTrue(any("--verify" in c for c in commands))
            self.assertIn("__entitlements", swift)
            signed = next(c for c in commands if "--sign" in c)
            self.assertNotIn("--entitlements", signed)
            entitlements = plistlib.loads((out / "simulator-entitlements.plist").read_bytes())
            self.assertEqual(entitlements["application-identifier"], "PHUXSIMDEV." + qualify.BUNDLE_ID)
            self.assertEqual(entitlements["keychain-access-groups"], ["PHUXSIMDEV." + qualify.BUNDLE_ID])
            self.assertEqual(app, out / "Phux Rune.app")

    def test_command_runner_is_bounded_and_rooted(self):
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            with patch.object(build.subprocess, "run") as run:
                build.run(["a tool", "a path"], {"SAFE": "yes"}, out, "test")
            self.assertEqual(run.call_args.args[0], ["a tool", "a path"])
            self.assertEqual(run.call_args.kwargs["cwd"], build.REPO)
            self.assertEqual(run.call_args.kwargs["timeout"], 120)
            self.assertNotIn("shell", run.call_args.kwargs)

    def test_release_device_rust_and_swift_targets(self):
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            library = out / "cargo/aarch64-apple-ios/release/libphux_rune_ios.a"
            library.parent.mkdir(parents=True)
            library.touch()
            with patch.object(build, "run", return_value="native-static-libs: -lSystem") as run:
                result = build.build_rust(out, {}, "aarch64-apple-ios", True)
            self.assertEqual(result, (library, ["-lSystem"], "release"))
            self.assertIn("--release", run.call_args.args[0])
            with patch.object(build, "run", side_effect=["/device/sdk\n", ""]) as run:
                build.build_swift(out, {}, out / "App", [], library, ["-lSystem"], False, True)
            command = run.call_args.args[0]
            self.assertIn("arm64-apple-ios26.0", command)
            self.assertIn("iphoneos", command)
            self.assertIn("-O", command)


class QualificationTests(unittest.TestCase):
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
        command = qualify.marker_command()
        self.assertNotIn(qualify.MARKER, command)
        result = subprocess.run(["/bin/sh", "-c", command], check=True,
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.stdout, qualify.MARKER + "\n")

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
            lane.simctl = Mock(side_effect=["", temporary, f"{qualify.BUNDLE_ID}: 123\n",
                f"123\t0\tUIKitApplication:{qualify.BUNDLE_ID}[test]\n", "-\t1\tother\n"])
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
            lane.require_alive(123)
            with self.assertRaises(ValueError):
                lane.require_alive(123)

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
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            container = root / "container"
            documents = container / "Documents"
            documents.mkdir(parents=True)
            (documents / "rune-qualification.json").write_text(json.dumps(valid_report()))
            (documents / "rune-tree.json").write_text(json.dumps({"title": qualify.SESSION}))
            lane = self.lane(root)
            lane.documents = documents
            lane.require_alive = Mock()

            def simctl(*args, **kwargs):
                if args[0] == "launch" and args[-1] == "com.apple.Preferences":
                    (documents / "rune-background.json").write_text('{"background_frames":4}')
                if args[0] == "launch" and args[-1] == qualify.BUNDLE_ID:
                    (documents / "rune-lifecycle.json").write_text(json.dumps({"background_frames":4,
                        "resumed_frames":5,"marker_seen":True}))
                if args[0] == "io":
                    Path(args[-1]).write_bytes(b"screenshot")
                return ""

            lane.simctl = Mock(side_effect=simctl)
            with patch.object(qualify.time, "sleep") as sleep:
                self.assertEqual(lane.collect_report(123), valid_report())
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
