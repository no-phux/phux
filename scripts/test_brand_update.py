import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
spec = importlib.util.spec_from_file_location("update_brand", Path(__file__).with_name("update-brand.py"))
update = importlib.util.module_from_spec(spec)
spec.loader.exec_module(update)


class BrandUpdateTests(unittest.TestCase):
    def test_semantic_version_order_prevents_automatic_downgrades(self):
        self.assertTrue(update.newer("1.10.0", "1.9.0"))
        self.assertFalse(update.newer("1.0.0", "2.0.0"))
        self.assertFalse(update.newer("1.0.0", "1.0.0"))

    def test_release_requires_immutability_and_github_asset_digest(self):
        release = {"draft": False, "prerelease": False, "immutable": True,
                   "tag_name": "v1.0.0", "assets": [{"name": "phux-brand.zip", "digest": "sha256:" + "a" * 64}]}
        with patch.object(update, "api", return_value=release):
            self.assertEqual(update.release(None), ("v1.0.0", "a" * 64))
        for field in ("draft", "prerelease", "immutable"):
            value = {**release, field: not release[field]}
            with self.subTest(field=field), patch.object(update, "api", return_value=value):
                with self.assertRaises(ValueError):
                    update.release(None)
        with patch.object(update, "api", return_value={**release, "assets": []}):
            with self.assertRaisesRegex(ValueError, "checksummed"):
                update.release(None)

    def test_untrusted_version_is_rejected_before_any_api_call(self):
        with patch.object(update, "api") as api:
            with self.assertRaises(ValueError):
                update.release("../../main")
            api.assert_not_called()

    def test_public_archive_is_append_only_and_retains_old_versions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "source.zip"
            archive.write_bytes(b"approved bytes")
            checksum = update.brand.digest(archive.read_bytes())
            manifest = {"version": "1.0.0", "source_revision": "a" * 40}
            update.public_kit(root, archive, checksum, manifest)
            update.public_kit(root, archive, checksum, {**manifest, "version": "1.1.0"})
            index = json.loads((root / "public/brand/releases/index.json").read_bytes())
            self.assertEqual(set(index["releases"]), {"1.0.0", "1.1.0"})
            archive.write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "already-published"):
                update.public_kit(root, archive, update.brand.digest(archive.read_bytes()), manifest)


if __name__ == "__main__":
    unittest.main()
