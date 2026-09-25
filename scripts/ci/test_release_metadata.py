#!/usr/bin/env python3
"""Release-metadata diffs must not request a compile lane. Anything else must."""

from pathlib import Path
import json
import os
import subprocess
import tempfile
import unittest

import release_metadata

ROOT = Path(__file__).resolve().parents[2]
RELEASE_PATHS = (
    "CHANGELOG.md",
    "Cargo.toml",
    "Cargo.lock",
    "clients/phux-web/Cargo.lock",
    ".release-please-manifest.json",
    "docs/site/worker/Dockerfile",
    ".agents/skills/using-phux/SKILL.md",
    ".agents/skills/using-phux-mcp/SKILL.md",
)


def bump(text, old="0.45.0", new="0.46.0"):
    return text.replace(old, new)


class ReleaseMetadataTests(unittest.TestCase):
    def test_shipped_release_files_are_version_only(self):
        pairs = {}
        for path in RELEASE_PATHS:
            before = (ROOT / path).read_text()
            after = bump(before)
            self.assertNotEqual(before, after, path)
            pairs[path] = (before, after)
        self.assertTrue(release_metadata.only(list(RELEASE_PATHS), pairs.get))

    def test_dependency_manifest_edit_still_compiles(self):
        before = (ROOT / "Cargo.toml").read_text()
        after = before.replace('serde = { version = "1"', 'serde = { version = "2"', 1)
        self.assertNotEqual(before, after)
        self.assertFalse(release_metadata.workspace_manifest_only(before, after))
        self.assertFalse(release_metadata.only(["Cargo.toml"], lambda _path: (before, after)))

    def test_lockfile_checksum_edit_still_compiles(self):
        before = (ROOT / "Cargo.lock").read_text()
        after = before.replace("checksum = ", "checksum = \"deadbeef", 1)
        self.assertFalse(release_metadata.workspace_lock_only(before, after))

    def test_skill_body_edit_still_compiles(self):
        before = (ROOT / ".agents/skills/using-phux/SKILL.md").read_text()
        after = before.replace("Use phux when", "Use something else when", 1)
        self.assertFalse(release_metadata.skill_only(before, after))

    def test_unknown_path_and_missing_side_fail_closed(self):
        pair = ("version = \"0.1.0\"\n", "version = \"0.2.0\"\n")
        self.assertFalse(release_metadata.only(["crates/phux/src/main.rs"], lambda _path: pair))
        self.assertFalse(release_metadata.only(["Cargo.toml"], lambda _path: None))
        self.assertFalse(release_metadata.only([], lambda _path: pair))

    def test_skip_outputs_cover_the_classifier(self):
        classified = subprocess.run(
            ["python3", str(ROOT / "scripts/ci/classify-changes.py")],
            input="", text=True, capture_output=True, check=True,
        )
        wanted = {line.split("=", 1)[0] for line in classified.stdout.splitlines()}
        emitted = {}
        for line in release_metadata.skip_outputs():
            key, value = line.split("=", 1)
            emitted[key] = value
        self.assertTrue(wanted <= set(emitted))
        for key in wanted:
            if key.endswith("_needed") or key in {"docs_only", "workflow_only", "e2e_needed"}:
                self.assertEqual(emitted[key], "false", key)
        self.assertEqual(emitted["unit_mode"], "skip")
        self.assertEqual(emitted["release_metadata_only"], "true")


class DetectSkipTests(unittest.TestCase):
    def test_version_bump_event_skips_compile_lanes(self):
        with tempfile.TemporaryDirectory(prefix="phux-release-metadata-") as temporary:
            repo = Path(temporary)
            git = lambda *args: subprocess.check_output(["git", *args], cwd=repo, text=True)
            git("init", "--quiet")
            git("config", "user.email", "routing@example.invalid")
            git("config", "user.name", "Routing Test")
            git("config", "core.hooksPath", "/dev/null")
            files = {
                "CHANGELOG.md": "# changelog\n",
                "Cargo.toml": "[workspace.package]\nversion = \"0.1.0\"\n",
                "Cargo.lock": "version = 3\n\n[[package]]\nname = \"phux\"\nversion = \"0.1.0\"\n",
                ".release-please-manifest.json": json.dumps({".": "0.1.0"}) + "\n",
                "docs/site/worker/Dockerfile": "ARG PHUX_VERSION=0.1.0\n",
                ".agents/skills/using-phux/SKILL.md": "  version: \"0.1.0\" # x-release-please-version\n",
            }
            for name, text in files.items():
                path = repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            git("add", "--all")
            git("commit", "--quiet", "-m", "base")
            base = git("rev-parse", "HEAD").strip()
            for name, text in files.items():
                (repo / name).write_text(text.replace("0.1.0", "0.2.0"))
            git("add", "--all")
            git("commit", "--quiet", "-m", "chore: release main")
            head = git("rev-parse", "HEAD").strip()
            with tempfile.NamedTemporaryFile(mode="w", suffix=".json") as payload:
                json.dump({"before": base, "after": head}, payload)
                payload.flush()
                result = subprocess.run(
                    ["python3", str(ROOT / "scripts/ci/detect-changes.py")],
                    cwd=repo,
                    env={**os.environ, "GITHUB_EVENT_NAME": "push", "GITHUB_EVENT_PATH": payload.name},
                    capture_output=True, text=True, check=True,
                )
        outputs = dict(line.split("=", 1) for line in result.stdout.splitlines())
        self.assertEqual(outputs["release_metadata_only"], "true")
        self.assertEqual(outputs["phux_needed"], "false")
        self.assertEqual(outputs["cockpit_needed"], "false")
        self.assertEqual(outputs["ffi_needed"], "false")
        self.assertEqual(outputs["cli_needed"], "false")
        self.assertEqual(outputs["zig_needed"], "false")
        self.assertIn("compile lanes skipped", result.stderr)


if __name__ == "__main__":
    unittest.main()
