"""Ad-hoc distribution stays fail-closed; device install remains explicit."""
import datetime
import hashlib
import importlib.util
from pathlib import Path
import plistlib
import runpy
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

path = Path(__file__).resolve().parents[1] / "ios/package_ota.py"
spec = importlib.util.spec_from_file_location("rune_ota", path)
ota = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ota)

NOW = datetime.datetime(2026, 11, 8, tzinfo=datetime.timezone.utc)
IDENTITY = hashlib.sha1(b"certificate").hexdigest().upper()


def profile():
    return {"ExpirationDate": datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
            + datetime.timedelta(days=365),
            "TeamIdentifier": ["TEAM"], "ProvisionedDevices": ["owned-device"],
            "DeveloperCertificates": [b"certificate"],
            "Entitlements": {"application-identifier": "TEAM." + ota.BUNDLE_ID,
                             "get-task-allow": False}}


class OtaTests(unittest.TestCase):
    def test_bundle_identity_matches_native_build_and_qualification(self):
        for script in ["build.py", "qualify.py"]:
            self.assertEqual(runpy.run_path(str(path.with_name(script)))["BUNDLE_ID"], ota.BUNDLE_ID)

    def test_exact_unexpired_profile_authorizes_local_identity(self):
        value = profile()
        self.assertEqual(ota.authorized_entitlements(value, IDENTITY, NOW), value["Entitlements"])

    def test_requested_device_must_actually_be_in_profile(self):
        ota.require_devices(profile(), ["owned-device"])
        for requested in [[], ["another-phone"], ["owned-device", "another-phone"]]:
            with self.subTest(requested=requested), self.assertRaises(ValueError):
                ota.require_devices(profile(), requested)

    def test_rejects_expired_store_enterprise_development_and_wrong_certificate(self):
        for field, value in [("ExpirationDate", datetime.datetime(2020, 1, 1)),
                             ("ProvisionedDevices", []), ("ProvisionsAllDevices", True),
                             ("DeveloperCertificates", [b"another certificate"]),
                             ("TeamIdentifier", ["OTHER"])]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                ota.authorized_entitlements({**profile(), field: value}, IDENTITY, NOW)
        value = profile()
        value["Entitlements"]["get-task-allow"] = True
        with self.assertRaises(ValueError):
            ota.authorized_entitlements(value, IDENTITY, NOW)

    def test_rejects_wildcard_or_other_application(self):
        for identifier in ["TEAM.*", "TEAM.other", "OTHER." + ota.BUNDLE_ID]:
            value = profile()
            value["Entitlements"]["application-identifier"] = identifier
            with self.subTest(identifier=identifier), self.assertRaises(ValueError):
                ota.authorized_entitlements(value, IDENTITY, NOW)

    def test_https_url_without_credentials_or_query_is_required(self):
        for url in ["http://host/a", "https:///a", "https://user:pass@host/a", "https://@host/a",
                    "https://host/a?secret=x", "https://host/a#fragment"]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                ota.checked_base_url(url)
        self.assertEqual(ota.checked_base_url("https://host/private"), "https://host/private/")

    def test_manifest_uses_exact_bundle_and_private_https_assets(self):
        manifest, page = ota.site_documents("https://host/private", "0.1.5")
        self.assertEqual(plistlib.loads(plistlib.dumps(manifest)), manifest)
        item = manifest["items"][0]
        self.assertEqual(item["assets"][0]["url"], "https://host/private/PhuxRune.ipa")
        self.assertEqual(item["metadata"]["bundle-identifier"], ota.BUNDLE_ID)
        self.assertEqual(item["metadata"]["bundle-version"], "0.1.5")
        self.assertIn("itms-services://?action=download-manifest&amp;url=", page)
        self.assertIn("https%3A%2F%2Fhost%2Fprivate%2Fmanifest.plist", page)

    def test_missing_device_list_and_extra_signed_entitlements_fail_closed(self):
        value = profile()
        del value["ProvisionedDevices"]
        with self.assertRaises(ValueError):
            ota.authorized_entitlements(value, IDENTITY, NOW)
        with tempfile.TemporaryDirectory() as temporary:
            app, embedded, out = self.bundle(Path(temporary))
            value = profile()
            extra = {**value["Entitlements"], "get-task-allow": True}
            with patch.object(ota, "run", side_effect=[plistlib.dumps(value), b"platform IOS\n",
                    b"", b"", plistlib.dumps(extra)]), self.assertRaisesRegex(ValueError, "differ"):
                ota.package(app, embedded, IDENTITY, "https://host/private/", out, ["owned-device"])

    def bundle(self, root):
        app = root / "unsigned.app"
        app.mkdir()
        (app / "Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": ota.BUNDLE_ID, "CFBundleExecutable": "PhuxRune",
            "CFBundleVersion": "0.1.5", "CFBundleSupportedPlatforms": ["iPhoneOS"]}))
        (app / "PhuxRune").write_bytes(b"actual-device-binary-fixture")
        embedded = root / "profile.mobileprovision"
        embedded.write_bytes(b"signed-Apple-profile-fixture")
        return app, embedded, root / "output"

    def test_package_preserves_executable_profile_entitlements_and_ipa_layout(self):
        with tempfile.TemporaryDirectory() as temporary:
            app, embedded, out = self.bundle(Path(temporary))
            value = profile()
            with patch.object(ota, "run", side_effect=[plistlib.dumps(value), b"platform IOS\n",
                    b"", b"", plistlib.dumps(value["Entitlements"])]) as run:
                site = ota.package(app, embedded, IDENTITY, "https://host/private/", out, ["owned-device"])
            self.assertEqual({p.name for p in site.iterdir()}, {"PhuxRune.ipa", "manifest.plist", "index.html"})
            self.assertIn("--generate-entitlement-der", run.call_args_list[2].args)
            with zipfile.ZipFile(site / "PhuxRune.ipa") as archive:
                self.assertEqual(archive.read("Payload/Phux Rune.app/PhuxRune"), b"actual-device-binary-fixture")
                self.assertEqual(archive.read("Payload/Phux Rune.app/embedded.mobileprovision"), embedded.read_bytes())
            self.assertEqual(plistlib.loads((out / "entitlements.plist").read_bytes()), value["Entitlements"])

    def test_device_bundle_rejects_simulator_symlinks_and_missing_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            app, _, _ = self.bundle(Path(temporary))
            with patch.object(ota, "run", return_value=b"platform IOSSIMULATOR\n"), self.assertRaises(ValueError):
                ota.device_bundle_info(app)
            (app / "linked-resource").symlink_to(app / "PhuxRune")
            with patch.object(ota, "run", return_value=b"platform IOS\n"), self.assertRaisesRegex(ValueError, "symlink"):
                ota.device_bundle_info(app)
            (app / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": ota.BUNDLE_ID}))
            with self.assertRaisesRegex(ValueError, "metadata"):
                ota.device_bundle_info(app)

    def test_tool_failure_preserves_diagnostic_without_mixing_successful_plist_stdout(self):
        failure = subprocess.CalledProcessError(1, ["codesign"], stderr=b"keychain locked")
        with patch.object(ota.subprocess, "check_output", side_effect=failure), self.assertRaisesRegex(RuntimeError, "keychain locked"):
            ota.run("codesign")


if __name__ == "__main__":
    unittest.main()
