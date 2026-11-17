"""A stable private page follows complete, correctly addressed phone builds."""
from functools import partial
import http.client
from http.server import ThreadingHTTPServer
from pathlib import Path
import plistlib
import sys
import tempfile
import threading
import unittest
import urllib.parse
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ios"))
import serve_ota as downloads
from package_ota import site_documents

BASE = "https://private.example/phone/"


class DownloadTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scheduled").mkdir()

    def publish(self, name, version="0.1.33", build="1.41.3", bundle=None, platform=None):
        child = self.root / "scheduled" / name
        child.mkdir()
        manifest, page = site_documents(BASE + "scheduled/" + name + "/", build)
        (child / "manifest.plist").write_bytes(plistlib.dumps(manifest))
        (child / "index.html").write_text(page)
        info = {"CFBundleIdentifier": bundle or downloads.BUNDLE_ID,
                "CFBundleVersion": build, "CFBundleShortVersionString": version,
                "CFBundleSupportedPlatforms": platform or ["iPhoneOS"]}
        with zipfile.ZipFile(child / "PhuxRune.ipa", "w") as archive:
            archive.writestr(downloads.INFO, plistlib.dumps(info))
        return child

    def latest(self):
        return downloads.latest_release(self.root, BASE)

    def test_numeric_marketing_then_build_order_not_directory_name_or_mtime(self):
        self.publish("zzz", "0.1.9", "99")
        self.publish("aaa", "0.1.10", "1.41.9")
        self.publish("bbb", "0.1.10", "1.41.10")
        result = self.latest()
        self.assertEqual((result["version"], result["build"]), ("0.1.10", "1.41.10"))
        self.assertEqual(result["url"], BASE + "scheduled/bbb/")
        self.assertEqual(urllib.parse.parse_qs(urllib.parse.urlsplit(result["install"]).query),
                         {"action": ["download-manifest"],
                          "url": [BASE + "scheduled/bbb/manifest.plist"]})

    def test_partial_or_mismatched_new_builds_do_not_replace_usable_build(self):
        self.publish("usable")
        incomplete = self.publish("partial", "0.1.99", "2")
        (incomplete / "PhuxRune.ipa").write_bytes(b"incomplete ZIP copy")
        mismatch = self.publish("mismatch", "0.1.99", "3")
        manifest = plistlib.loads((mismatch / "manifest.plist").read_bytes())
        manifest["items"][0]["metadata"]["bundle-version"] = "4"
        (mismatch / "manifest.plist").write_bytes(plistlib.dumps(manifest))
        self.publish("wrong-bundle", "0.1.99", "5", bundle="another.app")
        self.publish("simulator", "0.1.99", "6", platform=["iPhoneSimulator"])
        self.assertEqual(self.latest()["url"], BASE + "scheduled/usable/")

    def test_external_asset_addresses_and_symlinked_files_are_not_install_candidates(self):
        self.publish("usable")
        external = self.publish("external", "0.1.99", "2")
        manifest = plistlib.loads((external / "manifest.plist").read_bytes())
        manifest["items"][0]["assets"][0]["url"] = "https://elsewhere.example/PhuxRune.ipa"
        (external / "manifest.plist").write_bytes(plistlib.dumps(manifest))
        linked = self.publish("linked", "0.1.99", "3")
        (linked / "PhuxRune.ipa").unlink()
        (linked / "PhuxRune.ipa").symlink_to(self.root / "scheduled/usable/PhuxRune.ipa")
        (self.root / "scheduled/alias").symlink_to(external, target_is_directory=True)
        self.assertEqual(self.latest()["url"], BASE + "scheduled/usable/")

    def test_empty_catalog_has_no_install_action(self):
        self.assertIsNone(self.latest())
        page = downloads.download_page(None)
        self.assertIn("No phone build published yet", page)
        self.assertNotIn("itms-services:", page)

    def test_live_page_updates_without_server_restart_and_keeps_immutable_assets(self):
        first = self.publish("first")
        handler = partial(downloads.Handler, directory=self.root, base_url=BASE)
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(thread.join, 5)
        self.addCleanup(server.shutdown)

        def request(path, method="GET"):
            connection = http.client.HTTPConnection(*server.server_address, timeout=5)
            try:
                connection.request(method, path)
                response = connection.getresponse()
                return response.status, response.headers, response.read()
            finally:
                connection.close()

        status, headers, body = request("/scheduled/")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertIn(b"Version 0.1.33", body)
        self.publish("next", "0.1.34", "1.41.4")
        status, headers, body = request("/scheduled/")
        self.assertEqual(status, 200)
        self.assertIn(b"Version 0.1.34", body)
        self.assertIn(b"scheduled%2Fnext%2Fmanifest.plist", body)
        status, head_headers, head_body = request("/scheduled/index.html", "HEAD")
        self.assertEqual((status, head_body), (200, b""))
        self.assertEqual(int(head_headers["Content-Length"]), len(body))
        status, headers, _ = request("/scheduled")
        self.assertEqual((status, headers["Location"]), (308, BASE + "scheduled/"))
        status, headers, asset = request("/scheduled/first/PhuxRune.ipa")
        self.assertEqual((status, headers["Content-Type"]), (200, "application/octet-stream"))
        self.assertEqual(asset, (first / "PhuxRune.ipa").read_bytes())


if __name__ == "__main__":
    unittest.main()
