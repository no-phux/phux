"""Exercise source pin checks against real local Git repositories."""

import hashlib
import json
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest


BOOTSTRAP = runpy.run_path(str(Path(__file__).with_name("bootstrap-source.py")))
VERIFY = BOOTSTRAP["verify_source"]
PREPARE_PATCHES = BOOTSTRAP["prepare_patches"]


def repository(path: Path) -> str:
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    return commit(path)


def commit(path: Path) -> str:
    subprocess.run(
        ["git", "-C", str(path), "-c", "user.name=Test", "-c",
         "user.email=test@example.invalid", "-c", "commit.gpgsign=false",
         "commit", "-qm", "fixture", "--allow-empty", "--no-verify"],
        check=True,
    )
    return subprocess.check_output(
        ["git", "-C", str(path), "rev-parse", "HEAD"], text=True
    ).strip()


class SourcePinTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="phux-desktop-source-")
        self.addCleanup(self.directory.cleanup)
        self.source = Path(self.directory.name)
        self.pin = {
            "revision": repository(self.source),
            "zedRevision": repository(self.source / "zed"),
            "version": "0.10.0",
            "sha256": {"bun.lock": hashlib.sha256(b"fixture").hexdigest()},
        }
        (self.source / "bun.lock").write_bytes(b"fixture")
        for package in ("native", "solid"):
            directory = self.source / "packages" / package
            directory.mkdir(parents=True)
            (directory / "package.json").write_text('{"version":"0.10.0"}')
        (self.source / ".gitignore").write_text("/zed/\n")
        subprocess.run(["git", "-C", str(self.source), "add", "."], check=True)
        self.pin["revision"] = commit(self.source)

    def test_matched_source_is_accepted(self) -> None:
        VERIFY(self.source, self.pin)

    def test_changed_lockfile_is_rejected_and_preserved(self) -> None:
        (self.source / "bun.lock").write_bytes(b"local changes")
        with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
            VERIFY(self.source, self.pin)
        self.assertEqual((self.source / "bun.lock").read_bytes(), b"local changes")

    def test_wrong_revision_is_rejected(self) -> None:
        self.pin["revision"] = "0" * 40
        with self.assertRaisesRegex(SystemExit, "Unexpected GPUIX revision"):
            VERIFY(self.source, self.pin)

    def test_wrong_zed_revision_is_rejected(self) -> None:
        self.pin["zedRevision"] = "0" * 40
        with self.assertRaisesRegex(SystemExit, "Zed revision"):
            VERIFY(self.source, self.pin)

    def test_mixed_adapter_versions_are_rejected(self) -> None:
        manifest = self.source / "packages" / "solid" / "package.json"
        manifest.write_text(json.dumps({"version": "0.9.0"}))
        with self.assertRaisesRegex(SystemExit, "Mismatched @gpuix/solid"):
            VERIFY(self.source, self.pin)

    def test_unstaged_source_is_rejected_and_preserved(self) -> None:
        manifest = self.source / "packages" / "solid" / "package.json"
        content = '{"version":"0.10.0","scripts":{"build":"unreviewed"}}'
        manifest.write_text(content)
        with self.assertRaisesRegex(SystemExit, "Modified source"):
            VERIFY(self.source, self.pin)
        self.assertEqual(manifest.read_text(), content)

    def test_staged_source_is_rejected(self) -> None:
        manifest = self.source / "packages" / "solid" / "package.json"
        manifest.write_text('{"version":"0.10.0","modified":true}')
        subprocess.run(["git", "-C", str(self.source), "add", str(manifest)], check=True)
        with self.assertRaisesRegex(SystemExit, "Modified source"):
            VERIFY(self.source, self.pin)

    def test_zed_source_is_rejected_and_preserved(self) -> None:
        source = self.source / "zed" / "lib.rs"
        source.write_text("// pinned fixture\n")
        subprocess.run(["git", "-C", str(source.parent), "add", "."], check=True)
        self.pin["zedRevision"] = commit(source.parent)
        source.write_text("// local edit\n")
        with self.assertRaisesRegex(SystemExit, "Modified source"):
            VERIFY(self.source, self.pin)
        self.assertEqual(source.read_text(), "// local edit\n")

    def test_untracked_source_is_rejected(self) -> None:
        (self.source / "extra.ts").write_text("export const unreviewed = true;\n")
        with self.assertRaisesRegex(SystemExit, "Modified source"):
            VERIFY(self.source, self.pin)

    def extension_patch(self) -> Path:
        scratch = tempfile.TemporaryDirectory(prefix="phux-source-patch-")
        self.addCleanup(scratch.cleanup)
        patch = Path(scratch.name) / "extension.patch"
        patch.write_text(
            "diff --git a/extension.rs b/extension.rs\n"
            "new file mode 100644\n"
            "--- /dev/null\n+++ b/extension.rs\n@@ -0,0 +1 @@\n"
            "+// reviewed native extension\n"
        )
        return patch

    def test_reviewed_patch_applies_idempotently_without_staging(self) -> None:
        patches = [self.extension_patch()]
        PREPARE_PATCHES(self.source, patches)
        PREPARE_PATCHES(self.source, patches)
        self.assertEqual((self.source / "extension.rs").read_text(), "// reviewed native extension\n")
        staged = subprocess.check_output(
            ["git", "-C", str(self.source), "diff", "--cached", "--name-only"], text=True
        )
        self.assertEqual(staged, "")

    def test_modified_patch_output_is_rejected_and_preserved(self) -> None:
        patches = [self.extension_patch()]
        PREPARE_PATCHES(self.source, patches)
        source = self.source / "extension.rs"
        source.write_text("// user edit\n")
        with self.assertRaisesRegex(SystemExit, "beyond pinned patches"):
            PREPARE_PATCHES(self.source, patches)
        self.assertEqual(source.read_text(), "// user edit\n")

    def test_unrelated_file_blocks_patch_without_mutation(self) -> None:
        (self.source / "user.rs").write_text("// preserve\n")
        with self.assertRaisesRegex(SystemExit, "beyond pinned patches"):
            PREPARE_PATCHES(self.source, [self.extension_patch()])
        self.assertFalse((self.source / "extension.rs").exists())

    def test_exact_patch_prefix_can_advance_to_new_revision(self) -> None:
        first = self.extension_patch()
        second = first.with_name("second.patch")
        second.write_text(
            "diff --git a/extension.rs b/extension.rs\n"
            "--- a/extension.rs\n+++ b/extension.rs\n@@ -1 +1 @@\n"
            "-// reviewed native extension\n+// reviewed multiwindow extension\n"
        )
        PREPARE_PATCHES(self.source, [first])
        PREPARE_PATCHES(self.source, [first, second])
        PREPARE_PATCHES(self.source, [first, second])
        self.assertEqual((self.source / "extension.rs").read_text(), "// reviewed multiwindow extension\n")

    def test_index_flags_cannot_hide_zed_source_edits(self) -> None:
        source = self.source / "zed" / "lib.rs"
        source.write_text("// reviewed\n")
        subprocess.run(["git", "-C", str(source.parent), "add", "."], check=True)
        self.pin["zedRevision"] = commit(source.parent)
        for flag in ("assume-unchanged", "skip-worktree"):
            with self.subTest(flag=flag):
                subprocess.run(["git", "-C", str(source.parent), "update-index", f"--{flag}", "lib.rs"], check=True)
                source.write_text("// hidden user edit\n")
                with self.assertRaisesRegex(SystemExit, "Modified source"):
                    VERIFY(self.source, self.pin)
                self.assertEqual(source.read_text(), "// hidden user edit\n")
                subprocess.run(["git", "-C", str(source.parent), "update-index", f"--no-{flag}", "lib.rs"], check=True)
                source.write_text("// reviewed\n")

    def test_ignored_collision_rejects_entire_suffix_without_mutation(self) -> None:
        first = self.extension_patch()
        second = first.with_name("collision.patch")
        second.write_text(first.read_text().replace("extension.rs", "blocked.rs"))
        ignored = self.source / "blocked.rs"
        ignored.write_text("// preserve ignored work\n")
        (self.source / ".git" / "info" / "exclude").write_text("blocked.rs\n")
        with self.assertRaisesRegex(SystemExit, "replace existing path"):
            PREPARE_PATCHES(self.source, [first, second])
        self.assertFalse((self.source / "extension.rs").exists())
        self.assertEqual(ignored.read_text(), "// preserve ignored work\n")

    def test_ignored_parent_obstructions_reject_before_mutation(self) -> None:
        first = self.extension_patch()
        second = first.with_name("parent.patch")
        second.write_text(first.read_text().replace("extension.rs", "blocked/new.rs"))
        blocked = self.source / "blocked"
        (self.source / ".git" / "info" / "exclude").write_text("blocked\n")
        for target in (None, "missing-directory", "packages"):
            with self.subTest(target=target):
                if target is None:
                    blocked.write_text("// preserve\n")
                else:
                    blocked.symlink_to(target)
                with self.assertRaisesRegex(SystemExit, "obstructing parent"):
                    PREPARE_PATCHES(self.source, [first, second])
                self.assertFalse((self.source / "extension.rs").exists())
                blocked.unlink()

    def test_rename_destination_collision_rejects_before_mutation(self) -> None:
        first = self.extension_patch()
        second = first.with_name("rename.patch")
        second.write_text(
            "diff --git a/packages/native/package.json b/renamed.json\n"
            "similarity index 100%\nrename from packages/native/package.json\n"
            "rename to renamed.json\n"
        )
        subprocess.run(["git", "-C", str(self.source), "config", "diff.renames", "true"], check=True)
        (self.source / ".git" / "info" / "exclude").write_text("renamed.json\n")
        destination = self.source / "renamed.json"
        destination.write_text("user contents\n")
        with self.assertRaisesRegex(SystemExit, "replace existing path"):
            PREPARE_PATCHES(self.source, [first, second])
        self.assertFalse((self.source / "extension.rs").exists())
        self.assertTrue((self.source / "packages/native/package.json").exists())
        self.assertEqual(destination.read_text(), "user contents\n")


if __name__ == "__main__":
    unittest.main()
