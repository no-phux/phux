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
            name = rm.asset_name("desktop", "next", None, sha, "aarch64-apple-darwin")
            Path(dist, name).write_bytes(b"app")
            digest = hashlib.sha256(b"app").hexdigest()
            Path(dist, f"{name}.sha256").write_text(f"{digest}  {name}\n")
            with self.assertRaisesRegex(rm.ManifestError, "not signed"):
                rm.verified_dist("desktop", sha, dist)
            Path(dist, f"{name}.sha256.minisig").write_text("signature\n")
            self.assertEqual(rm.verified_dist("desktop", sha, dist)["aarch64-apple-darwin"][1], digest)
            Path(dist, f"{name}.sha256").write_text(f"{'0' * 64}  {name}\n")
            with self.assertRaises(rm.ManifestError):
                rm.verified_dist("desktop", sha, dist)

    def test_bridge_records_keep_the_pre_manifest_shape(self):
        name, text = rm.bridge_record("desktop", "c" * 40, "1.2.3",
                                      {"aarch64-apple-darwin": "d" * 64})
        record = json.loads(text)
        self.assertEqual(name, "desktop-channel.json")
        self.assertEqual(record["archive"], f"phux-desktop-next.{'c' * 40}-macos-arm64.zip")
        self.assertEqual(rm.bridge_record("phux", "c" * 40, "1.2.3", {})[0], "channel.json")


if __name__ == "__main__":
    unittest.main()
