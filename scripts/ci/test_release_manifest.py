#!/usr/bin/env python3
"""The release manifest generator (ADR-0175) and its rendered contract."""

import json
import re
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import release_manifest as rm  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
# The only line shapes the POSIX installers accept.
LINE = re.compile(
    r'^(\{|\s*\},?|\s*"[A-Za-z0-9_-]+": \{|\s*"[A-Za-z0-9_-]+": ("[A-Za-z0-9._:/+-]*"|[0-9]+),?)$'
)


def digests(value="e" * 64):
    return {target: value for target in rm.PHUX_TARGETS}


class RenderTests(unittest.TestCase):
    def test_committed_fixtures_are_current(self):
        self.assertEqual(rm.main(["fixtures", "--check"]), 0)

    def test_rendered_form_is_line_parseable(self):
        for channel in rm.CHANNELS:
            text = rm.render(rm.fixture(channel))
            self.assertNotIn("[", text)
            for line in text.splitlines():
                self.assertRegex(line, LINE)
            self.assertEqual(json.loads(text), rm.fixture(channel))

    def test_unsafe_values_and_empty_objects_are_refused(self):
        manifest = rm.fixture("stable")
        manifest["products"]["phux"]["version"] = "1.2.3 evil"
        with self.assertRaises(rm.ManifestError):
            rm.render(manifest)
        with self.assertRaises(rm.ManifestError):
            rm.render(rm.build("stable", {}, "2026-01-01T00:00:00Z"))
        with self.assertRaises(rm.ManifestError):
            rm.render({"flag": True})


class EntryTests(unittest.TestCase):
    def test_stable_and_next_names_and_urls_are_derived(self):
        stable = rm.product_entry("phux", "stable", "1.2.3", "a" * 40, "0.9", digests())
        artifact = stable["artifacts"]["x86_64-unknown-linux-gnu"]
        self.assertEqual(stable["release"], "v1.2.3")
        self.assertEqual(artifact["name"], "phux-v1.2.3-x86_64-unknown-linux-gnu.tar.gz")
        self.assertEqual(
            artifact["url"],
            "https://github.com/no-phux/phux/releases/download/v1.2.3/"
            "phux-v1.2.3-x86_64-unknown-linux-gnu.tar.gz",
        )
        desktop = rm.product_entry(
            "desktop", "next", "0.4.0", "b" * 40, "0.9",
            {rm.DESKTOP_TARGET: "f" * 64}, "27.0")
        artifact = desktop["artifacts"][rm.DESKTOP_TARGET]
        self.assertEqual(desktop["release"], "next")
        self.assertEqual(artifact["name"], f"phux-desktop-next.{'b' * 40}-macos-arm64.zip")
        self.assertTrue(artifact["url"].endswith(f"/download/next/{artifact['name']}"))
        self.assertEqual(list(desktop), ["version", "release", "sha", "protocol",
                                         "minimum_macos", "artifacts"])

    def test_malformed_inputs_are_refused(self):
        good = ("phux", "stable", "1.2.3", "a" * 40, "0.9", digests())
        for index, bad in [(2, "1.02.3"), (2, "1.2"), (3, "A" * 40), (4, "0.09")]:
            args = list(good)
            args[index] = bad
            with self.assertRaises(rm.ManifestError, msg=bad):
                rm.product_entry(*args)
        with self.assertRaises(rm.ManifestError):
            rm.product_entry(*good[:5], {"aarch64-apple-darwin": "e" * 64})
        with self.assertRaises(rm.ManifestError):
            rm.product_entry(*good[:5], digests("E" * 64))


class PublishedStateTests(unittest.TestCase):
    def test_newest_stable_is_numeric_and_ignores_other_streams(self):
        rows = [
            ["v0.9.0", False, False],
            ["v0.10.0", False, False],
            ["v0.11.0", True, False],
            ["v0.12.0-rc.1", False, True],
            ["desktop-v0.3.0", False, False],
            ["desktop-v0.20.0", False, False],
            ["desktop-v0.21.0-alpha.1", False, False],
            ["pi-extension-v9.0.0", False, False],
            ["next", False, True],
            ["channels", False, True],
        ]
        self.assertEqual(rm.newest_stable(rows), {
            "phux": ("0.10.0", "v0.10.0"),
            "desktop": ("0.20.0", "desktop-v0.20.0"),
        })

    def test_sidecars_name_their_artifact(self):
        name = "phux-v1.2.3-x86_64-unknown-linux-gnu.tar.gz"
        self.assertEqual(rm.sidecar_digest(f"{'e' * 64}  {name}\n", name), "e" * 64)
        self.assertEqual(rm.sidecar_digest(f"{'e' * 64} *{name}\n", name), "e" * 64)
        with self.assertRaises(rm.ManifestError):
            rm.sidecar_digest(f"{'e' * 64}  other.tar.gz\n", name)

    def test_generated_at_alone_is_not_a_change(self):
        left = rm.fixture("next")
        right = dict(left, generated_at="2030-01-01T00:00:00Z")
        self.assertTrue(rm.same_content(left, right))
        right["channel"] = "stable"
        self.assertFalse(rm.same_content(left, right))

    def test_protocol_is_read_from_the_source_it_ships(self):
        self.assertEqual(rm.protocol_at("HEAD", "desktop"),
                         (ROOT / "clients/desktop/phux-protocol").read_text().strip())
        self.assertRegex(rm.protocol_at("HEAD", "phux"), rm.PROTOCOL)


class WorkflowTests(unittest.TestCase):
    def test_both_channels_regenerate_and_publish(self):
        publish = (ROOT / ".github/workflows/publish.yml").read_text()
        self.assertIn("needs: [plan, phux, desktop]", publish)
        self.assertIn("python3 scripts/ci/release_manifest.py stable --out", publish)
        nxt = (ROOT / ".github/workflows/next-release.yml").read_text()
        self.assertIn("needs: [gate, publish]", nxt)
        self.assertIn("python3 scripts/ci/release_manifest.py next --out", nxt)
        for workflow in (publish, nxt):
            self.assertIn('.json" --publish', workflow)


class FailureTests(unittest.TestCase):
    def test_a_failure_is_recorded_not_mistaken_for_absence(self):
        def broken():
            raise subprocess.CalledProcessError(1, ["gh"], "", "HTTP 502")

        products, failures = rm.gather([
            ("phux", broken),
            ("desktop", lambda: None),
        ])
        self.assertEqual(products, {})
        self.assertEqual(list(failures), ["phux"])
        self.assertIn("HTTP 502", failures["phux"])

    def test_a_failed_product_keeps_its_published_entry(self):
        previous = rm.fixture("stable")
        fresh_desktop = dict(previous["products"]["desktop"], version="1.2.4")
        kept = rm.carry_forward({"desktop": fresh_desktop}, {"phux": "HTTP 502"}, previous)
        self.assertEqual(list(kept), ["phux", "desktop"])
        self.assertEqual(kept["phux"], previous["products"]["phux"])
        self.assertEqual(kept["desktop"]["version"], "1.2.4")
        # With nothing published before, the failed product stays absent.
        self.assertEqual(list(rm.carry_forward({}, {"phux": "x"}, None)), [])


if __name__ == "__main__":
    unittest.main()
