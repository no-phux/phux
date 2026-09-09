#!/usr/bin/env python3
"""An exact-input artifact hit requires both source identity and every byte."""

import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import cockpit_artifacts as artifacts


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        root = Path(self.scratch.name)
        self.source, self.cache, self.target = root / "source", root / "cache", root / "target"
        self.source.mkdir()
        for name in artifacts.FILES:
            (self.source / name).write_bytes(name.encode())
        (self.source / "phux").chmod(0o755)
        self.identity = {"tree": "a" * 40, "profile": "ffi-release"}
        artifacts.save(self.identity, self.source, self.cache)

    def test_exact_outputs_and_executable_mode_survive(self):
        self.assertTrue(artifacts.restore(self.identity, self.cache, self.target))
        self.assertEqual((self.target / "phux").read_bytes(), b"phux")
        self.assertEqual((self.target / "phux").stat().st_mode & 0o777, 0o755)

    def test_tree_or_profile_mismatch_does_not_install(self):
        for identity in [{**self.identity, "tree": "b" * 40}, {**self.identity, "profile": "release"}]:
            with self.assertRaises(ValueError):
                artifacts.restore(identity, self.cache, self.target)
            self.assertFalse(self.target.exists())

    def test_corruption_does_not_install_either_artifact(self):
        (self.cache / "phux").write_bytes(b"corrupted")
        self.assertFalse(artifacts.restore(self.identity, self.cache, self.target))
        self.assertFalse(self.target.exists())

    def test_manifest_cannot_add_an_output_path(self):
        path = self.cache / "manifest.json"
        manifest = json.loads(path.read_text())
        manifest["files"]["../bad"] = "a" * 64
        path.write_text(json.dumps(manifest))
        with self.assertRaises(ValueError):
            artifacts.restore(self.identity, self.cache, self.target)

    def test_verify_checks_current_outputs_without_writing(self):
        artifacts.restore(self.identity, self.cache, self.target)
        before = {name: (self.target / name).stat() for name in artifacts.FILES}
        self.assertTrue(artifacts.verify(self.identity, self.cache, self.target))
        for name in artifacts.FILES:
            after = (self.target / name).stat()
            self.assertEqual(after.st_ino, before[name].st_ino)
            self.assertEqual(after.st_mtime_ns, before[name].st_mtime_ns)
        (self.target / "phux").write_bytes(b"unexpected replacement")
        self.assertFalse(artifacts.verify(self.identity, self.cache, self.target))
        self.assertEqual((self.target / "phux").read_bytes(), b"unexpected replacement")

    def test_restore_replaces_complete_file_without_truncating_open_reader(self):
        artifacts.restore(self.identity, self.cache, self.target)
        output = self.target / "phux"
        output.write_bytes(b"old in-use binary")
        with output.open("rb") as active_reader:
            self.assertTrue(artifacts.restore(self.identity, self.cache, self.target))
            self.assertEqual(active_reader.read(), b"old in-use binary")
        self.assertEqual(output.read_bytes(), b"phux")

    def test_dirty_checkout_cannot_claim_committed_tree_identity(self):
        with patch.object(artifacts, "command", return_value=" M crates/phux/src/main.rs"):
            with self.assertRaisesRegex(ValueError, "clean checkout"):
                artifacts.build_identity()

    def test_engine_optimization_is_a_build_input(self):
        def command(*args):
            return "" if args[:2] == ("git", "status") else "fixture"

        with (patch.object(artifacts, "command", side_effect=command),
              patch.object(artifacts.shutil, "which", return_value="/compiler/zig"),
              patch.object(artifacts, "digest", return_value="compiler-fingerprint"),
              patch.dict(os.environ, {}, clear=True)):
            default_key = artifacts.cache_key(artifacts.build_identity())
            os.environ["LIBGHOSTTY_VT_SYS_OPTIMIZE"] = "Debug"
            self.assertNotEqual(default_key, artifacts.cache_key(artifacts.build_identity()))

    def test_external_engine_directories_cannot_claim_tree_identity(self):
        for key in ("GHOSTTY_SOURCE_DIR", "GHOSTTY_ZIG_SYSTEM_DIR"):
            with self.subTest(key=key), patch.object(artifacts, "command", return_value=""):
                with patch.dict(os.environ, {key: "/external/unversioned"}, clear=True):
                    with self.assertRaisesRegex(ValueError, "external Ghostty"):
                        artifacts.build_identity()


if __name__ == "__main__":
    unittest.main()
