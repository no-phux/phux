"""Native framework, privacy and notice packaging contracts."""
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import patch

from test_ios_build import build, load


class NativeAssetTests(unittest.TestCase):
    def test_parsed_command_output_is_never_contaminated_by_diagnostics(self):
        for helper in (build, load("tailnet")):
            with self.subTest(helper=helper.__name__), tempfile.TemporaryDirectory() as temporary:
                out = Path(temporary)
                command = [sys.executable, "-c", "import sys; print('/a path'); print('warning: diagnostic', file=sys.stderr)"]
                value = helper.run(command, {}, out, "parsed", stdout_only=True)
                self.assertEqual(value, "/a path\n")
                self.assertEqual((out / "parsed.stderr.log").read_text(), "warning: diagnostic\n")
                combined = helper.run(command, {}, out, "compiler")
                self.assertIn("/a path", combined)
                self.assertIn("warning: diagnostic", combined)

    def test_tailnet_framework_resources_notices_and_inside_out_simulator_signing(self):
        for simulator in (True, False):
            with self.subTest(simulator=simulator), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                app = root / "App"
                app.mkdir()
                (app / "THIRD-PARTY-NOTICES.txt").write_text("Cargo notices\n")
                framework = root / "TailscaleKit.framework"
                framework.mkdir()
                (framework / "TailscaleKit").write_bytes(b"fixture")
                resource = root / "PrivacyInfo.xcprivacy"
                resource.write_text("privacy fixture")
                bundle = root / "NIOPrivacy.bundle"
                bundle.mkdir()
                (bundle / "Info.plist").write_text("bundle fixture")
                payload = {"frameworks": [framework], "resources": [resource, bundle],
                           "notices": "Swift and Go notices\n"}
                with patch.object(build, "run") as run:
                    build.embed_tailnet(app, payload, {}, root, simulator)
                self.assertTrue((app / "Frameworks/TailscaleKit.framework/TailscaleKit").is_file())
                self.assertTrue((app / resource.name).is_file())
                self.assertTrue((app / bundle.name / "Info.plist").is_file())
                self.assertEqual((app / "THIRD-PARTY-NOTICES.txt").read_text(),
                                 "Cargo notices\n\nSwift and Go notices\n")
                self.assertEqual(run.call_count, int(simulator))
                if simulator:
                    self.assertNotIn("--entitlements", run.call_args.args[0])
                    self.assertEqual(run.call_args.args[0][-1], app / "Frameworks/TailscaleKit.framework")

    def test_notices_are_resolved_from_native_bridge_not_desktop(self):
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            app = out / "App"
            app.mkdir()
            with patch.object(build.shutil, "copy2"), patch.object(
                    build, "run", side_effect=['{"resolve": {}}', "native notices\n"]) as run:
                fonts = build.copy_resources(app, {}, out, "aarch64-apple-ios-sim")
            metadata = run.call_args_list[0].args[0]
            self.assertEqual(metadata[-1], build.IOS / "bridge/Cargo.toml")
            self.assertIn("--locked", metadata)
            self.assertIn("phux-rune-ios", run.call_args_list[1].args[0][2])
            self.assertEqual((app / "THIRD-PARTY-NOTICES.txt").read_text(), "native notices\n")
            self.assertIn("Inter.ttf", fonts)

    def test_privacy_manifest_covers_first_party_preferences_and_embedded_runtime(self):
        manifest = plistlib.loads((build.IOS / "Tailnet/PrivacyInfo.xcprivacy").read_bytes())
        reasons = {item["NSPrivacyAccessedAPIType"]: item["NSPrivacyAccessedAPITypeReasons"]
                   for item in manifest["NSPrivacyAccessedAPITypes"]}
        for category, reason in [("UserDefaults", "CA92.1"), ("FileTimestamp", "C617.1"), ("SystemBootTime", "35F9.1")]:
            self.assertIn(reason, reasons["NSPrivacyAccessedAPICategory" + category])
        self.assertFalse(manifest["NSPrivacyTracking"])


if __name__ == "__main__":
    unittest.main()
