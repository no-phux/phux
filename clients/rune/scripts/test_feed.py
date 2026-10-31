"""feed.py's release rules. Run by scripts/check.sh."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

import feed

HERE = os.path.dirname(os.path.abspath(__file__))
CARGO = 'name = "phux-rune"\nversion = "0.1.0-alpha.8"\n'
NOTES = "**Phux Rune 0.1.0-alpha.8**: words.\n"


class Ordering(unittest.TestCase):
    def test_a_release_sorts_after_its_alphas(self):
        tags = ["rune-v0.1.0-alpha.7", "rune-v0.1.0", "rune-v0.1.0-alpha.10"]
        self.assertEqual(feed.newest(tags), "rune-v0.1.0")

    def test_alphas_sort_as_numbers(self):
        tags = ["rune-v0.1.0-alpha.9", "rune-v0.1.0-alpha.10", "rune-v0.1.0-alpha.2"]
        self.assertEqual(feed.newest(tags), "rune-v0.1.0-alpha.10")

    def test_the_next_alpha_beats_the_last_release(self):
        tags = ["rune-v0.1.0", "rune-v0.1.1-alpha.1", "rune-v0.1.0-alpha.3"]
        self.assertEqual(feed.newest(tags), "rune-v0.1.1-alpha.1")

    def test_numbers_are_not_compared_as_text(self):
        self.assertEqual(feed.newest(["rune-v0.9.0", "rune-v0.10.0"]), "rune-v0.10.0")

    def test_other_products_are_ignored(self):
        tags = ["desktop-v9.0.0", "v9.0.0", "next", "channels", "rune-v0.1.0"]
        self.assertEqual(feed.newest(tags), "rune-v0.1.0")

    def test_a_tag_clients_would_refuse_fails(self):
        for suspect in ["rune-v0.1.0-beta.1", "rune-v0.1", "rune-v01.0.0", "rune-v0.1.0-alpha.01"]:
            with self.assertRaisesRegex(ValueError, "order"):
                feed.newest(["rune-v0.1.0", suspect])

    def test_no_tag_fails(self):
        with self.assertRaises(ValueError):
            feed.newest(["v1.0.0"])


class Preflight(unittest.TestCase):
    def test_a_matching_tag_passes(self):
        self.assertEqual(feed.preflight("rune-v0.1.0-alpha.8", CARGO, NOTES), [])

    def test_the_manifest_must_match(self):
        problems = feed.preflight("rune-v0.1.0-alpha.9", CARGO, "**Phux Rune 0.1.0-alpha.9**")
        self.assertEqual(len(problems), 1)
        self.assertIn("Cargo.toml", problems[0])

    def test_the_notes_must_name_the_tag(self):
        problems = feed.preflight("rune-v0.1.0-alpha.8", CARGO, "**Phux Rune 0.1.0-alpha.7**")
        self.assertEqual(len(problems), 1)
        self.assertIn("RELEASE_NOTES", problems[0])

    def test_a_longer_version_does_not_satisfy_the_header(self):
        notes = "**Phux Rune 0.1.0-alpha.80**"
        self.assertTrue(feed.preflight("rune-v0.1.0-alpha.8", CARGO, notes))

    def test_a_beta_tag_never_builds(self):
        manifest = 'version = "0.1.0-beta.1"\n'
        problems = feed.preflight("rune-v0.1.0-beta.1", manifest, "**Phux Rune 0.1.0-beta.1**")
        self.assertIn("clients refuse", problems[0])


class ReleaseFlags(unittest.TestCase):
    def test_an_alpha_is_a_prerelease(self):
        self.assertEqual(
            feed.release_flags("0.1.0-alpha.8"),
            ["--title", "Phux Rune 0.1.0-alpha.8 (alpha)", "--prerelease"],
        )

    def test_a_release_is_not(self):
        self.assertEqual(feed.release_flags("0.1.0"), ["--title", "Phux Rune 0.1.0"])


class Build(unittest.TestCase):
    def test_the_feed_reads_the_sidecars(self):
        with tempfile.TemporaryDirectory() as dist:
            for suffix in feed.ARCHIVES.values():
                name = f"phux-rune-0.1.0-{suffix}"
                with open(os.path.join(dist, name), "wb") as archive:
                    archive.write(b"four")
                with open(os.path.join(dist, f"{name}.sha256"), "w", encoding="utf-8") as sidecar:
                    sidecar.write(f"{'a' * 64}  {name}\n")
            source = os.path.join(dist, "lib.rs")
            with open(source, "w", encoding="utf-8") as lib:
                lib.write("PROTOCOL_VERSION: Version = Version {\n major: 3,\n minor: 4,\n")
            script = os.path.join(HERE, "feed.py")
            output = subprocess.run(
                [sys.executable, script, "build", "0.1.0", "abc", dist, source],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        document = json.loads(output)
        self.assertEqual(document["protocol"], "3.4")
        self.assertEqual(document["targets"]["aarch64-apple-darwin"]["size"], 4)
        self.assertEqual(document["targets"]["x86_64-unknown-linux-gnu"]["sha256"], "a" * 64)


class CommandLine(unittest.TestCase):
    def test_newest_reads_stdin_and_a_suspect_tag_exits_nonzero(self):
        script = os.path.join(HERE, "feed.py")
        good = subprocess.run(
            [sys.executable, script, "newest"],
            input="rune-v0.1.0-alpha.7\nrune-v0.1.0\n",
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(good.stdout.strip(), "rune-v0.1.0")
        bad = subprocess.run(
            [sys.executable, script, "newest"],
            input="rune-v0.1.0-beta.1\n",
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(bad.returncode, 0)


if __name__ == "__main__":
    unittest.main()
