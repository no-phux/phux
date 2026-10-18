#!/usr/bin/env python3
"""The channel manifest generator (ADR-0176)."""

import hashlib
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import release_manifest as rm  # noqa: E402

# Every line shape the POSIX installers' awk parser accepts.
LINE = re.compile(
    r'^(\{|\s*\},?|\s*"[A-Za-z0-9_-]+": \{|\s*"[A-Za-z0-9_-]+": ("[A-Za-z0-9._:/+-]*"|[0-9]+),?)$'
)


def row(tag, assets=(), draft=False, prerelease=False):
    return {"tag_name": tag, "draft": draft, "prerelease": prerelease,
            "assets": [{"name": name, "digest": digest} for name, digest in assets]}


class ManifestTests(unittest.TestCase):
    def test_desktop_names_cover_both_linux_architectures_on_both_channels(self):
        for channel in rm.CHANNELS:
            document = rm.fixture(channel)["products"]["desktop"]
            identity = "1.2.3" if channel == "stable" else f"next.{'b' * 40}"
            for target, suffix in rm.DESKTOP_ARCHIVES.items():
                artifact = document["artifacts"][target]
                self.assertEqual(artifact["name"], f"phux-desktop-{identity}-{suffix}")
                self.assertTrue(artifact["url"].endswith("/" + artifact["name"]))
            self.assertEqual(set(document["artifacts"]), set(rm.TARGETS["desktop"]))

    def test_desktop_entry_requires_all_platforms(self):
        with self.assertRaisesRegex(rm.ManifestError, "exactly"):
            rm.entry("desktop", "stable", "1.2.3", "a" * 40, "0.9",
                     {"aarch64-apple-darwin": "d" * 64})

    def test_fixtures_are_current_and_line_parseable(self):
        self.assertEqual(rm.main(["fixtures", "--check"]), 0)
        for channel in rm.CHANNELS:
            text = rm.render(rm.fixture(channel))
            self.assertTrue(all(LINE.match(line) for line in text.splitlines()), text)

    def test_render_refuses_what_the_installers_cannot_parse(self):
        bad = rm.fixture("stable")
        bad["products"]["phux"]["version"] = "1.2.3 evil"
        for document in (bad, {"flag": True}, {"list": [1]}, {"empty": {}}):
            with self.assertRaises(rm.ManifestError):
                rm.render(document)

    def test_entry_refuses_malformed_facts(self):
        digests = {t: "e" * 64 for t in rm.TARGETS["phux"]}
        good = ["phux", "stable", "1.2.3", "a" * 40, "0.9", digests]
        for index, value in [(2, "1.02.3"), (3, "A" * 40), (4, "0.09"),
                             (5, {"aarch64-apple-darwin": "e" * 64}),
                             (5, {t: "E" * 64 for t in rm.TARGETS["phux"]})]:
            args = list(good)
            args[index] = value
            with self.assertRaises(rm.ManifestError, msg=value):
                rm.entry(*args)


class StableTests(unittest.TestCase):
    def test_desktop_stable_requires_uploaded_assets_for_every_target(self):
        from unittest.mock import patch

        assets = [(rm.asset_name("desktop", "stable", "1.2.3", None, target), "sha256:" + "d" * 64)
                  for target in rm.TARGETS["desktop"]]
        with patch.object(rm, "tag_commit", return_value="a" * 40), \
                patch.object(rm, "protocol_at", return_value="0.9"):
            entry = rm.stable_entry("desktop", row("desktop-v1.2.3", assets))
            self.assertEqual(set(entry["artifacts"]), set(rm.TARGETS["desktop"]))
            with self.assertRaisesRegex(rm.ManifestError, "not published"):
                rm.stable_entry("desktop", row("desktop-v1.2.3", assets[:-1]))

    def test_newest_published_release_per_product(self):
        rows = [row("v0.9.0"), row("v0.10.0"), row("v0.11.0", draft=True),
                row("desktop-v0.3.0"), row("desktop-v0.20.0"),
                row("desktop-v0.1.0-alpha.17"), row("pi-extension-v9.0.0"),
                row("next", prerelease=True), row("channels", prerelease=True)]
        newest = rm.newest_stable(rows)
        self.assertEqual({p: r["tag_name"] for p, r in newest.items()},
                         {"phux": "v0.10.0", "desktop": "desktop-v0.20.0"})

    def test_digests_come_from_github_and_must_exist(self):
        name = rm.asset_name("phux", "stable", "1.2.3", None, "x86_64-unknown-linux-gnu")
        self.assertEqual(name, "phux-v1.2.3-x86_64-unknown-linux-gnu.tar.gz")
        assets = [{"name": name, "digest": "sha256:" + "e" * 64}]
        self.assertEqual(rm.asset_digests(assets, [name]), {name: "e" * 64})
        for broken in ([], [{"name": name, "digest": None}]):
            with self.assertRaises(rm.ManifestError):
                rm.asset_digests(broken, [name])


class NextTests(unittest.TestCase):
    def test_built_assets_must_match_their_sidecars(self):
        sha = "c" * 40
        with tempfile.TemporaryDirectory() as dist:
            digest = hashlib.sha256(b"app").hexdigest()
            for target in rm.TARGETS["desktop"]:
                archive = rm.asset_name("desktop", "next", None, sha, target)
                Path(dist, archive).write_bytes(b"app")
                Path(dist, f"{archive}.sha256").write_text(f"{digest}  {archive}\n")
                Path(dist, f"{archive}.sha256.minisig").write_text("signature\n")
            name = rm.asset_name("desktop", "next", None, sha, "aarch64-apple-darwin")
            Path(dist, f"{name}.sha256.minisig").unlink()
            with self.assertRaisesRegex(rm.ManifestError, "not signed"):
                rm.verified_dist("desktop", sha, dist)
            Path(dist, f"{name}.sha256.minisig").write_text("signature\n")
            self.assertEqual(rm.verified_dist("desktop", sha, dist)["aarch64-apple-darwin"][1], digest)
            Path(dist, f"{name}.sha256").write_text(f"{'0' * 64}  {name}\n")
            with self.assertRaises(rm.ManifestError):
                rm.verified_dist("desktop", sha, dist)

    def test_missing_linux_archive_or_signature_prevents_publication(self):
        sha = "c" * 40
        with tempfile.TemporaryDirectory() as dist:
            for target in rm.TARGETS["desktop"]:
                name = rm.asset_name("desktop", "next", None, sha, target)
                Path(dist, name).write_bytes(b"app")
                digest = hashlib.sha256(b"app").hexdigest()
                Path(dist, f"{name}.sha256").write_text(f"{digest}  {name}\n")
                Path(dist, f"{name}.sha256.minisig").write_text("signature\n")
            self.assertEqual(set(rm.verified_dist("desktop", sha, dist)), set(rm.TARGETS["desktop"]))
            Path(dist, f"{name}.sha256.minisig").unlink()
            with self.assertRaisesRegex(rm.ManifestError, "not signed"):
                rm.verified_dist("desktop", sha, dist)
            Path(dist, name).unlink()
            with self.assertRaises(FileNotFoundError):
                rm.verified_dist("desktop", sha, dist)

    def test_bridge_records_keep_the_pre_manifest_shape(self):
        name, text = rm.bridge_record("desktop", "c" * 40, "1.2.3",
                                      {"aarch64-apple-darwin": "d" * 64})
        record = json.loads(text)
        self.assertEqual(name, "desktop-channel.json")
        self.assertEqual(record["archive"], f"phux-desktop-next.{'c' * 40}-macos-arm64.zip")
        self.assertEqual(rm.bridge_record("phux", "c" * 40, "1.2.3", {})[0], "channel.json")


class DesktopWorkflowTests(unittest.TestCase):
    """Keep every release platform behind the same qualification/signing gate."""

    def test_linux_builds_and_qualifies_natively_in_pr_and_release_lanes(self):
        for name in ("desktop-test.yml", "desktop-release.yml"):
            workflow = (rm.ROOT / ".github/workflows" / name).read_text()
            linux = workflow.split("\n  linux:\n", 1)[1].split("\n  # Signing", 1)[0]
            for required in ("os: ubuntu-24.04\n", "os: ubuntu-24.04-arm\n",
                             "target: x86_64-unknown-linux-gnu", "target: aarch64-unknown-linux-gnu",
                             "just desktop-test", "just desktop-native-build", "package-linux.ts",
                             "tar -xzf", "smoke-linux.ts", "xvfb-run", "dbus-run-session"):
                self.assertIn(required, linux, f"{name}: {required}")

    def test_stable_signer_waits_for_every_platform_and_signs_all_sidecars(self):
        workflow = (rm.ROOT / ".github/workflows/desktop-release.yml").read_text()
        signer = workflow.split("\n  sign:\n", 1)[1]
        self.assertIn("needs: [macos, linux]", signer)
        self.assertIn("pattern: desktop-sidecar-*", signer)
        self.assertIn('test "${#sidecars[@]}" -eq 3', signer)
        self.assertIn('bash scripts/ci/sign-release.sh "${sidecars[@]}"', signer)
        self.assertIn('cat "${sidecars[@]}"', signer)
        for platform in ("macos-arm64", "linux-${{ matrix.arch }}"):
            self.assertIn(f"name: desktop-sidecar-{platform}", workflow)
            self.assertIn(f"name: desktop-{platform}", workflow)

    def test_next_rebuilds_when_desktop_only_inputs_change_and_signs_linux(self):
        workflow = (rm.ROOT / ".github/workflows/next-release.yml").read_text()
        self.assertIn("changed desktop phux_needed native_needed desktop_needed", workflow)
        self.assertIn("merge-multiple: true", workflow)
        self.assertIn("sign-release.sh dist/*.sha256", workflow)


if __name__ == "__main__":
    unittest.main()
