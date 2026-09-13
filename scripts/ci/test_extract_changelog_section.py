#!/usr/bin/env python3
"""Linear notes must come from the tagged changelog section, not a SHA stub."""

import tempfile
import unittest
from pathlib import Path

from extract_changelog_section import extract_section, main, version_from_tag


CHANGELOG = """# Changelog

## [0.36.0](https://example/compare/v0.35.0...v0.36.0) (2026-09-13)

### Features

* fleet inbox

## [0.35.0](https://example/compare/v0.34.0...v0.35.0) (2026-09-13)

### Features

* quic streams
"""


class ExtractChangelogSectionTests(unittest.TestCase):
    def test_version_from_root_and_component_tags(self):
        self.assertEqual(version_from_tag("v0.36.0"), "0.36.0")
        self.assertEqual(version_from_tag("cockpit-v0.23.3"), "0.23.3")
        self.assertEqual(version_from_tag("opencode-plugin-v0.3.0"), "0.3.0")
        with self.assertRaises(ValueError):
            version_from_tag("next")

    def test_extracts_one_section_and_stops_at_the_next_heading(self):
        section = extract_section(CHANGELOG, "0.36.0")
        self.assertIn("fleet inbox", section)
        self.assertNotIn("quic streams", section)
        self.assertTrue(section.startswith("## [0.36.0]"))

    def test_missing_section_fails_closed(self):
        with self.assertRaisesRegex(ValueError, "no changelog heading"):
            extract_section(CHANGELOG, "9.9.9")

    def test_cli_writes_the_tagged_section(self):
        with tempfile.TemporaryDirectory() as tmp:
            changelog = Path(tmp) / "CHANGELOG.md"
            output = Path(tmp) / "notes.md"
            changelog.write_text(CHANGELOG, encoding="utf-8")
            self.assertEqual(
                main(["--tag", "v0.36.0", "--changelog", str(changelog), "--output", str(output)]),
                0,
            )
            self.assertEqual(output.read_text(encoding="utf-8"), extract_section(CHANGELOG, "0.36.0"))

    def test_real_root_changelog_has_the_current_head_section(self):
        root = Path(__file__).resolve().parents[2]
        text = (root / "CHANGELOG.md").read_text(encoding="utf-8")
        heading = next(line for line in text.splitlines() if line.startswith("## ["))
        version = heading.split("[", 1)[1].split("]", 1)[0]
        section = extract_section(text, version)
        self.assertTrue(section.startswith(f"## [{version}]"))


if __name__ == "__main__":
    unittest.main()
