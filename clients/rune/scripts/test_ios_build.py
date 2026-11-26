"""Focused driver contracts; real Swift/Rust/simulator execution is a CI gate."""

import importlib.util
import fnmatch
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch


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
            "composer_through_native": True,
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
            info = build.bundle_plist(build.preview_identity("1.2.3-beta"), simulator, ["Font.ttf"])
            self.assertEqual(info["CFBundleIdentifier"], qualify.BUNDLE_ID)
            self.assertEqual(info["MinimumOSVersion"], "26.0")
            self.assertEqual(info["CFBundleVersion"], "1.2.3")
            self.assertEqual(info["CFBundleSupportedPlatforms"], [platform])
            self.assertEqual(info["CFBundleURLTypes"][0]["CFBundleURLSchemes"], ["phux-rune"])
            self.assertIn("NSCameraUsageDescription", info)
            self.assertIs(info["ITSAppUsesNonExemptEncryption"], False)
            self.assertIn("NSLocalNetworkUsageDescription", info)
            self.assertEqual(info["UILaunchScreen"], {"UIColorName": "LaunchGround"})
            self.assertEqual(info["UIDeviceFamily"], [1])  # A portrait phone form only.
            self.assertIs(info["CADisableMinimumFrameDurationOnPhone"], True)
            self.assertEqual(plistlib.loads(plistlib.dumps(info)), info)
        icons = {"CFBundleIcons": {"CFBundlePrimaryIcon": {"CFBundleIconName": "AppIcon"}}}
        self.assertEqual(build.bundle_plist(build.preview_identity("1.0.0"), False, [], icons)["CFBundleIcons"], icons["CFBundleIcons"])

    def test_distributor_identity_replaces_the_preview_identity(self):
        identity = {"bundle_id": "dev.phux.mobile", "display_name": "phux",
                    "url_schemes": ["phux", "phux-rune"], "version": "0.5.0",
                    "build_number": "812"}
        info = build.bundle_plist(identity, False, [])
        self.assertEqual(info["CFBundleIdentifier"], "dev.phux.mobile")
        self.assertEqual((info["CFBundleName"], info["CFBundleDisplayName"]), ("phux", "phux"))
        self.assertEqual((info["CFBundleShortVersionString"], info["CFBundleVersion"]), ("0.5.0", "812"))
        self.assertEqual(info["CFBundleURLTypes"],
                         [{"CFBundleURLName": "dev.phux.mobile.pairing",
                           "CFBundleURLSchemes": ["phux", "phux-rune"]}])
        with tempfile.TemporaryDirectory() as temporary:
            entitlements = plistlib.loads(
                build.simulator_entitlements(Path(temporary), "dev.phux.mobile").read_bytes())
        self.assertEqual(entitlements["application-identifier"], "PHUXSIMDEV.dev.phux.mobile")

    def test_widgets_need_an_app_group_both_bundles_name(self):
        plain = build.bundle_plist(build.preview_identity("1.0.0"), False, [])
        self.assertNotIn("NSSupportsLiveActivities", plain)
        self.assertNotIn("PhuxAppGroup", plain)
        identity = {**build.preview_identity("1.0.0"), "bundle_id": "dev.phux.mobile",
                    "app_group": "group.dev.phux.mobile"}
        info = build.bundle_plist(identity, False, [])
        self.assertIs(info["NSSupportsLiveActivities"], True)
        widgets = load("widgets")
        extension = widgets.extension_plist(identity, False, build.MIN_IOS)
        self.assertEqual(extension["PhuxAppGroup"], info["PhuxAppGroup"])
        # The store refuses an extension outside its app's identifier or version.
        self.assertEqual(extension["CFBundleIdentifier"], "dev.phux.mobile.agentstatus")
        for key in ("CFBundleShortVersionString", "CFBundleVersion", "MinimumOSVersion"):
            self.assertEqual(extension[key], info[key])
        with tempfile.TemporaryDirectory() as temporary:
            entitlements = plistlib.loads(build.simulator_entitlements(
                Path(temporary), "dev.phux.mobile.agentstatus", "group.dev.phux.mobile").read_bytes())
        self.assertEqual(entitlements["com.apple.security.application-groups"], ["group.dev.phux.mobile"])
        self.assertEqual(build.app_group("group.dev.phux.mobile"), "group.dev.phux.mobile")
        for bad in ("dev.phux.mobile", "group.", "group"):
            with self.assertRaises(build.argparse.ArgumentTypeError):
                build.app_group(bad)

    def test_identity_arguments_reject_malformed_values(self):
        for parse, value in ((build.bundle_id, "nodots"), (build.bundle_id, "a..b"),
                             (build.url_scheme, "Phux"), (build.url_scheme, "1phux"),
                             (build.marketing_version, "v1"), (build.build_number, "1-beta"),
                             (build.display_name, "a/b"), (build.display_name, "")):
            with self.subTest(value=value), self.assertRaises(build.argparse.ArgumentTypeError):
                parse(value)
        self.assertEqual(build.build_number("812"), "812")
        self.assertEqual(build.marketing_version("0.5.0-rc.1"), "0.5.0-rc.1")

    def test_build_commands_real_staticlib_and_swift_link(self):
        with tempfile.TemporaryDirectory(prefix="rune path ") as temporary:
            root = Path(temporary)
            ios, rune, out = root / "ios", root / "rune", root / "out"
            (ios / "Sources").mkdir(parents=True)
            (ios / "Sources/Host.swift").touch()
            (ios / "Bridge.h").touch()
            rune.mkdir()
            (rune / "Cargo.toml").write_text('[package]\nversion = "1.2.3"\n')
            stale = rune / "target/aarch64-apple-ios-sim/debug/libphux_rune_ios.a"
            stale.parent.mkdir(parents=True)
            stale.write_bytes(b"stale")
            commands = []

            def fake_run(command, env, output, name, timeout=120, stdout_only=False):
                commands.append([str(arg) for arg in command])
                if name == "rust":
                    target = command[command.index("--target") + 1]
                    library = Path(env["CARGO_TARGET_DIR"]) / target / "debug/libphux_rune_ios.a"
                    self.assertEqual(library, stale)
                    self.assertFalse(library.exists())
                    library.parent.mkdir(parents=True, exist_ok=True)
                    library.write_bytes(b"fresh")
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
            linked = out / "cargo/aarch64-apple-ios-sim/debug/libphux_rune_ios.a"
            self.assertEqual(linked.read_bytes(), b"fresh")
            self.assertIn(str(linked), swift)

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

            def place(_command, env, *_rest, **_kwargs):
                produced = Path(env["CARGO_TARGET_DIR"]) / "aarch64-apple-ios/release/libphux_rune_ios.a"
                produced.parent.mkdir(parents=True)
                produced.touch()
                return "native-static-libs: -lSystem"

            with patch.object(build, "run", side_effect=place) as run:
                result = build.build_rust(out, {"CARGO_TARGET_DIR": str(out / "target")}, "aarch64-apple-ios", True)
            self.assertEqual(result, (library, ["-lSystem"], "release"))
            self.assertTrue(library.is_file())
            self.assertIn("--release", run.call_args.args[0])
            with patch.object(build, "run", side_effect=["/device/sdk\n", ""]) as run:
                build.build_swift(out, {}, out / "App", [], library, ["-lSystem"], False, True)
            command = run.call_args.args[0]
            self.assertIn("arm64-apple-ios26.0", command)
            self.assertIn("iphoneos", command)
            self.assertIn("-O", command)


if __name__ == "__main__":
    unittest.main()
