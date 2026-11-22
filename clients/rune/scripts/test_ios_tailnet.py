#!/usr/bin/env python3
"""Headless tests of the pinned, isolated iOS tailnet build helper."""

import importlib.util
import json
import os
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch
import zipfile

IOS = Path(__file__).resolve().parents[1] / "ios"
SPEC = importlib.util.spec_from_file_location("tailnet", IOS / "tailnet.py")
tailnet = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tailnet)


class TailnetBuildTests(unittest.TestCase):
    def test_manifest_and_lock_pin_exact_dependency_closure(self):
        manifest = (tailnet.PACKAGE / "Package.swift").read_text()
        self.assertIn(tailnet.ARTIFACT_SHA256, manifest)
        self.assertIn('exact: "2.104.0"', manifest)
        self.assertIn('exact: "0.15.0"', manifest)
        pins = json.loads((tailnet.PACKAGE / "Package.resolved").read_text())["pins"]
        self.assertEqual({pin["identity"] for pin in pins}, {
            "swift-asn1", "swift-atomics", "swift-collections", "swift-crypto",
            "swift-nio", "swift-nio-ssh", "swift-system",
        })
        self.assertTrue(all(len(pin["state"]["revision"]) == 40 for pin in pins))

    def test_warnings_are_errors_for_each_first_party_target_in_all_configurations(self):
        manifest = (tailnet.PACKAGE / "Package.swift").read_text()
        for kind, name in (("target", "RuneTailnet"), ("testTarget", "RuneTailnetTests")):
            with self.subTest(target=name):
                target = manifest.split(f'.{kind}(name: "{name}",', 1)[1].split("\n        .", 1)[0]
                self.assertIn('swiftSettings: [.unsafeFlags(["-warnings-as-errors"])]', target)
                self.assertNotIn(".when(", target)
                self.assertNotIn("-suppress-warnings", target)
        self.assertEqual(manifest.count('"-warnings-as-errors"'), 2)

    def test_ci_host_tests_do_not_forward_warning_flags_to_dependencies(self):
        workflow = (IOS.parents[2] / ".github/workflows/rune-build.yml").read_text()
        step = workflow.split("- name: Test native tailnet trust, SSH and cancellation", 1)[1]
        step = step.split("\n      - ", 1)[0]
        self.assertIn("swift test", step)
        self.assertNotIn("-warnings-as-errors", step)
        self.assertNotIn("-Xswiftc", step)

    def test_artifact_digest_is_checked_before_extracting(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            (out / "TailscaleKit.xcframework.zip").write_bytes(b"not the artifact")
            with patch.object(tailnet, "run") as run:
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    tailnet.artifact(out, {})
                run.assert_not_called()
            self.assertFalse((out / "artifact").exists())

    def test_checked_digest_is_required_for_supplied_offline_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            source = out / "untrusted.zip"
            source.write_bytes(b"untrusted")
            with patch.object(tailnet.urllib.request, "urlopen") as network:
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    tailnet.artifact(out, {"RUNE_TAILSCALE_ARCHIVE": str(source)})
                network.assert_not_called()

    def test_archive_cannot_escape_output_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "unsafe.zip"
            with zipfile.ZipFile(path, "w") as archive:
                archive.writestr("../outside", "bad")
            import hashlib
            checksum = hashlib.sha256(path.read_bytes()).hexdigest()
            with patch.object(tailnet, "ARTIFACT_SHA256", checksum):
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    tailnet.verify_archive(path)

    def test_framework_slice_is_platform_specific(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            libraries = []
            for variant in [None, "simulator"]:
                identifier = "ios-arm64" + ("-simulator" if variant else "")
                (root / identifier / "TailscaleKit.framework").mkdir(parents=True)
                item = {"SupportedPlatform": "ios", "SupportedArchitectures": ["arm64"],
                        "LibraryIdentifier": identifier, "LibraryPath": "TailscaleKit.framework"}
                if variant:
                    item["SupportedPlatformVariant"] = variant
                libraries.append(item)
            (root / "Info.plist").write_bytes(plistlib.dumps({"AvailableLibraries": libraries}))
            self.assertEqual(tailnet.framework_slice(root, "device").parent.name, "ios-arm64")
            self.assertEqual(tailnet.framework_slice(root, "simulator").parent.name, "ios-arm64-simulator")

    def test_prepare_integration_contract_and_isolation(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory).resolve()
            products = out / "products"
            products.mkdir()
            (products / "libRuneTailnet.a").touch()
            (products / "Modules/RuneTailnet.swiftmodule").mkdir(parents=True)
            bundle = products / "swift_nio_NIOPosix.bundle"
            bundle.mkdir()
            framework = out / "artifact/ios-arm64/TailscaleKit.framework"
            framework.mkdir(parents=True)
            commands = []

            def run(command, env, destination, name, timeout=1800, stdout_only=False):
                commands.append([str(value) for value in command])
                self.assertFalse(any(key.startswith("PHUX_") for key in env))
                self.assertNotIn("SDKROOT", env)  # SwiftPM manifests use the host SDK.
                self.assertTrue(str(env["CLANG_MODULE_CACHE_PATH"]).startswith(str(out)))
                return str(products) if name == "products" else "/SDK"

            with patch.object(tailnet, "artifact", return_value=out / "artifact"), \
                    patch.object(tailnet, "framework_slice", return_value=framework), \
                    patch.object(tailnet, "run", side_effect=run), \
                    patch.object(tailnet, "clang_module_flags", return_value=[]), \
                    patch.object(tailnet, "tailscale_notices", return_value="tailscale notices"), \
                    patch.object(tailnet, "notices", return_value="notices"):
                result = tailnet.prepare(out, {**os.environ, "PHUX_SOCKET": "production", "SDKROOT": "/inherited-ios-sdk"}, "device", False)
            self.assertEqual(result["frameworks"], [framework])
            self.assertIn(bundle, result["resources"])
            self.assertIn(out / "tailnet/package/PrivacyInfo.xcprivacy", result["resources"])
            self.assertIn(str(products / "libRuneTailnet.a"), result["swift_flags"])
            self.assertIn("@executable_path/Frameworks", result["swift_flags"])
            build = next(command for command in commands if "--product" in command)
            self.assertEqual(build[:5], ["/usr/bin/xcrun", "--sdk", "macosx", "swift", "build"])
            self.assertIn("--force-resolved-versions", build)
            self.assertIn("--disable-keychain", build)
            self.assertIn("arm64-apple-ios26.0", build)
            self.assertNotIn("-warnings-as-errors", build)
            self.assertNotIn("-Xswiftc", build)
            self.assertIn(str(out / "tailnet/build"), build)
            copied = (out / "tailnet/package/Package.swift").read_text()
            self.assertIn('path: "', copied)
            self.assertNotIn(tailnet.ARTIFACT_URL, copied)
            self.assertEqual(copied.count('swiftSettings: [.unsafeFlags(["-warnings-as-errors"])]'), 2)
            self.assertTrue((out / "tailnet/integration.json").is_file())

    def test_invalid_target_does_not_create_output(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / "absent"
            with self.assertRaisesRegex(ValueError, "target"):
                tailnet.prepare(out, {}, "other", False)
            self.assertFalse(out.exists())

    def test_license_manifest_pins_actual_artifact_closure(self):
        manifest = json.loads((tailnet.PACKAGE / "license-sources.json").read_text())
        self.assertEqual(manifest["artifact_revision"], tailnet.TAILSCALE_REVISION)
        self.assertEqual(manifest["go_version"], "1.27.2")
        self.assertEqual(len(manifest["modules"]), 30)
        modules = {module["name"]: module for module in manifest["modules"]}
        for name in ["libtailscale", "tailscale.com", "go", "github.com/tailscale/wireguard-go", "golang.org/x/sys"]:
            self.assertIn(name, modules)
        self.assertTrue(all(len(module["text_sha256"]) == 64 for module in modules.values()))
        for path in tailnet.PACKAGE.rglob("*"):
            if path.is_file():
                self.assertLess(path.stat().st_size, 20_000, str(path))

    def test_untrusted_license_text_is_not_published_or_cached(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            with patch.object(tailnet, "fetched_license", return_value="untrusted text"):
                with self.assertRaisesRegex(ValueError, "license checksum mismatch"):
                    tailnet.tailscale_notices(out)
            self.assertEqual(list((out / "licenses").iterdir()), [])

    def test_cached_license_is_reverified_before_reuse_without_network(self):
        manifest = json.loads((tailnet.PACKAGE / "license-sources.json").read_text())
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            cache = out / "licenses"
            cache.mkdir()
            (cache / (manifest["modules"][0]["text_sha256"] + ".txt")).write_text("altered")
            with patch.object(tailnet, "fetched_license") as fetch:
                with self.assertRaisesRegex(ValueError, "license checksum mismatch"):
                    tailnet.tailscale_notices(out)
                fetch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
