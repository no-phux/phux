import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

import bundle_tmd


def fixture(symlink=False):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as archive:
        for name in ("tmd", "LICENSE", "THIRD_PARTY_NOTICES.md", "../ignored"):
            member = tarfile.TarInfo(name)
            text = name.encode()
            member.size = len(text)
            if name == "tmd" and symlink:
                member.type = tarfile.SYMTYPE
                member.linkname = "/outside"
            archive.addfile(member, io.BytesIO(text))
    return data.getvalue()


class BundleTests(unittest.TestCase):
    def test_binary_and_all_notices_are_bundled_without_archive_paths(self):
        data = fixture()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle_tmd.unpack(data, hashlib.sha256(data).hexdigest(), root / "bin", root / "notices")
            self.assertEqual((root / "bin/tmd").read_bytes(), b"tmd")
            self.assertEqual((root / "bin/tmd").stat().st_mode & 0o777, 0o755)
            for name in ("tmd-LICENSE.txt", "tmd-THIRD-PARTY-NOTICES.md", "tmd-VERSION.txt"):
                self.assertTrue((root / "notices" / name).is_file())
            self.assertFalse((root / "ignored").exists())

    def test_wrong_digest_or_symlink_fails_before_writing(self):
        for symlink in (False, True):
            data = fixture(symlink)
            digest = hashlib.sha256(data).hexdigest() if symlink else "0" * 64
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with self.assertRaises(ValueError):
                    bundle_tmd.unpack(data, digest, root / "bin", root / "notices")
                self.assertFalse((root / "bin").exists())

    def test_every_desktop_target_is_pinned(self):
        self.assertEqual(set(bundle_tmd.DIGESTS), {
            ("Darwin", "arm64"), ("Linux", "aarch64"), ("Linux", "x86_64"),
        })
        for _, digest in bundle_tmd.DIGESTS.values():
            self.assertRegex(digest, r"^[a-f0-9]{64}$")

    def test_packaging_and_inside_out_signing_include_the_helper(self):
        repo = Path(__file__).resolve().parents[3]
        package = (repo / "clients/rune/scripts/package.sh").read_text()
        self.assertEqual(package.count('python3 "$here/scripts/bundle_tmd.py"'), 2)
        signing = (repo / "scripts/ci/sign-macos-desktop.sh").read_text()
        self.assertLess(signing.index("for helper in phux tmd"), signing.index('--timestamp "$app"'))
