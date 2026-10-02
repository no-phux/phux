#!/usr/bin/env python3
"""Regressions for release literals drifting away from their real source."""

import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

import site_source_pin as pin

LIB_REV = "b" * 40


def manifest(version):
    return f'''[workspace.package]
version = "{version}"
[workspace.dependencies]
libghostty-vt = {{ git = "https://github.com/phall1/libghostty-rs.git", rev = "{LIB_REV}" }}
'''.encode()


def lock(version):
    return (f'''version = 4

[[package]]
name = "phux"
version = "{version}"
''' + "".join(f'''
[[package]]
name = "{name}"
version = "0.1.0"
source = "{pin.LIBGHOSTTY_SOURCE}?rev={LIB_REV}#{LIB_REV}"
''' for name in ("libghostty-vt", "libghostty-vt-sys"))).encode()


def archive(revision, files):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as tar:
        for path, content in files.items():
            info = tarfile.TarInfo(f"phux-{revision}/{path}")
            info.size = len(content)
            tar.addfile(info, io.BytesIO(content))
    return data.getvalue()


def dockerfile(revision, version, archive_digest="c" * 64):
    values = pin.source_metadata(manifest(version), lock(version))
    values.update(PHUX_REVISION=revision, PHUX_SOURCE_SHA256=archive_digest)
    return "\n".join([
        *(f"ARG {name}={value}" for name, value in values.items()),
        f"ADD --checksum=sha256:{archive_digest} \\",
        f"    https://github.com/no-phux/phux/archive/{revision}.tar.gz /tmp/phux.tar.gz",
        *(f'RUN test "${name}" = "{values[name]}"' for name in ("PHUX_VERSION", "PHUX_REVISION", "PHUX_SOURCE_SHA256")),
        f"ARG PHUX_VERSION={version}",
        "",
    ])


class SourcePinTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="phux-source-pin-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "--quiet")
        self.git("config", "user.email", "pin@example.invalid")
        self.git("config", "user.name", "Pin Test")
        self.git("config", "core.hooksPath", "/dev/null")
        self.old = self.commit("0.47.0")
        self.current = self.commit("0.48.0")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()

    def commit(self, version):
        (self.root / "Cargo.toml").write_bytes(manifest(version))
        (self.root / "Cargo.lock").write_bytes(lock(version))
        self.git("add", ".")
        self.git("commit", "--quiet", "-m", f"version {version}")
        return self.git("rev-parse", "HEAD")

    def test_literal_only_release_bump_fails(self):
        # The #959/#958 failure: every PHUX_VERSION literal says 0.48.0,
        # while the checksummed immutable archive still builds 0.47.0.
        stale = dockerfile(self.old, "0.47.0").replace("0.47.0", "0.48.0")
        with self.assertRaisesRegex(ValueError, "is phux 0.47.0"):
            pin.check(self.root, stale)

    def test_complete_refresh_matches_archive_and_preserves_external_pins(self):
        before = dockerfile(self.old, "0.47.0").replace("0.47.0", "0.48.0")
        source = archive(self.current, {"Cargo.toml": manifest("0.48.0"), "Cargo.lock": lock("0.48.0")})
        after = pin.refresh(self.root, before, self.current, source)
        pin.check(self.root, after)
        args = pin.arguments(after)
        self.assertEqual(args["PHUX_REVISION"], self.current)
        self.assertEqual(args["PHUX_SOURCE_SHA256"], pin.digest(source))
        self.assertEqual(args["LIBGHOSTTY_REVISION"], LIB_REV)
        self.assertEqual(pin.refresh(self.root, after, self.current, source), after)

    def test_checks_original_and_patched_lock_digests(self):
        before = dockerfile(self.current, "0.48.0")
        for key in ("PHUX_LOCK_SHA256", "PHUX_PATCHED_LOCK_SHA256"):
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, key):
                pin.check(self.root, before.replace(pin.arguments(before)[key], "0" * 64))

    def test_mismatched_archive_is_never_written(self):
        source = archive(self.current, {"Cargo.toml": manifest("0.47.0"), "Cargo.lock": lock("0.48.0")})
        with self.assertRaisesRegex(ValueError, "differs from source commit"):
            pin.refresh(self.root, dockerfile(self.old, "0.47.0"), self.current, source)

    def test_archive_url_digest_and_assertions_cannot_drift(self):
        before = dockerfile(self.current, "0.48.0")
        for after in (
            before.replace(f"archive/{self.current}", f"archive/{self.old}"),
            before.replace("ADD --checksum=sha256:" + "c" * 64, "ADD --checksum=sha256:" + "d" * 64),
            before.replace('RUN test "$PHUX_REVISION"', 'RUN echo "$PHUX_REVISION"'),
            before + "ARG PHUX_VERSION=0.49.0\n",
        ):
            with self.subTest(after=after), self.assertRaises(ValueError):
                pin.check(self.root, after)

    def test_unsynchronized_lock_and_external_pin_fail_closed(self):
        with self.assertRaisesRegex(ValueError, "path versions"):
            pin.source_metadata(manifest("0.48.0"), lock("0.47.0"))
        with self.assertRaisesRegex(ValueError, "does not resolve"):
            pin.source_metadata(manifest("0.48.0"), lock("0.48.0").replace(LIB_REV.encode(), b"a" * 40))

    def test_local_workspace_lock_must_also_match(self):
        (self.root / "Cargo.lock").write_bytes(lock("0.47.0"))
        with self.assertRaisesRegex(ValueError, "path versions"):
            pin.check(self.root, dockerfile(self.current, "0.48.0"))

    def test_squash_merge_without_release_branch_uses_verified_archive(self):
        # A fresh clone after squash/rebase contains the squashed source but
        # not the release PR's original lock-sync commit.
        missing = "f" * 40
        source = archive(missing, {"Cargo.toml": manifest("0.48.0"), "Cargo.lock": lock("0.48.0")})
        docker = dockerfile(missing, "0.48.0", pin.digest(source))
        with mock.patch.object(pin, "download_archive", return_value=source) as fetch:
            pin.check(self.root, docker)
        fetch.assert_called_once_with(missing)

    def test_pin_only_commit_does_not_recursively_refresh(self):
        path = self.root / pin.DOCKERFILE
        path.parent.mkdir(parents=True)
        path.write_text(dockerfile(self.current, "0.48.0"))
        self.git("add", ".")
        self.git("commit", "--quiet", "-m", "pin source snapshot")
        with mock.patch.object(pin, "ROOT", self.root), mock.patch.object(pin, "download_archive") as fetch:
            with mock.patch("sys.argv", ["site_source_pin", "--revision", "HEAD"]):
                self.assertEqual(pin.main(), 0)
        fetch.assert_not_called()
        self.assertEqual(pin.arguments(path.read_text())["PHUX_REVISION"], self.current)

    def test_missing_commit_fallback_rejects_archive_checksum_mismatch(self):
        with mock.patch.object(pin, "download_archive", return_value=b"wrong archive"):
            with self.assertRaisesRegex(ValueError, "archive checksum"):
                pin.check(self.root, dockerfile("f" * 40, "0.48.0"))



if __name__ == "__main__":
    unittest.main()
