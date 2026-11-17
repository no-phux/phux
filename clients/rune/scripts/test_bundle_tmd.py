import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import subprocess

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
            ("Linux", "aarch64"), ("Linux", "x86_64"),
        })
        for _, digest in bundle_tmd.DIGESTS.values():
            self.assertRegex(digest, r"^[a-f0-9]{64}$")
        self.assertRegex(bundle_tmd.SOURCE_REV, r"^[a-f0-9]{40}$")
        self.assertRegex(bundle_tmd.SOURCE_SHA256, r"^[a-f0-9]{64}$")
        self.assertEqual(bundle_tmd.MACOS_TARGET, "aarch64-macos.14.0.0")

    def test_macos_build_uses_verified_source_and_the_pinned_compiler_for_14(self):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as archive:
            for name in ("LICENSE", "THIRD_PARTY_NOTICES.md"):
                member = tarfile.TarInfo(f"tmd-{bundle_tmd.SOURCE_REV}/{name}")
                member.size = len(name)
                archive.addfile(member, io.BytesIO(name.encode()))
        data = data.getvalue()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(bundle_tmd.subprocess, "run") as run:
                with self.assertRaisesRegex(ValueError, "source checksum"):
                    bundle_tmd.build_macos(data, root / "bin", root / "notices")
                run.assert_not_called()

            def compiler(command, **kwargs):
                if command == ["zig", "version"]:
                    repo = Path(__file__).resolve().parents[3]
                    import json
                    version = json.loads((repo / ".config/zig-toolchain.json").read_text())["version"]
                    return subprocess.CompletedProcess(command, 0, version + "\n")
                self.assertEqual(command, ["zig", "build", "--release=small", "-Dtarget=aarch64-macos.14.0.0"])
                output = kwargs["cwd"] / "zig-out/bin/tmd"
                output.parent.mkdir(parents=True)
                output.write_bytes(b"compiled tmd")

            with patch.object(bundle_tmd, "SOURCE_SHA256", hashlib.sha256(data).hexdigest()), \
                    patch.object(bundle_tmd.subprocess, "run", side_effect=compiler):
                bundle_tmd.build_macos(data, root / "bin", root / "notices")
            self.assertEqual((root / "bin/tmd").read_bytes(), b"compiled tmd")
            self.assertIn("Target: aarch64-macos.14.0.0", (root / "notices/tmd-VERSION.txt").read_text())

    def test_packaging_and_inside_out_signing_include_the_helper(self):
        repo = Path(__file__).resolve().parents[3]
        package = (repo / "clients/rune/scripts/package.sh").read_text()
        self.assertEqual(package.count('python3 "$here/scripts/bundle_tmd.py"'), 2)
        signing = (repo / "scripts/ci/sign-macos-desktop.sh").read_text()
        self.assertLess(signing.index("for helper in phux tmd"), signing.index('--timestamp "$app"'))
