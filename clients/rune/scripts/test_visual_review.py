"""Header-only fixtures qualify the packager, never an actual image decoder."""
import copy
import json
from itertools import product
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import visual_review as review


SOURCE = {
    "sourceRevision": "a" * 40, "currentRunePin": "b" * 40, "dirty": True,
    "gitDiffHeadSha256": "c" * 64, "untrackedSourceFiles": [],
}


class VisualReview(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.tree = {
            "schema": 1, "projection": "ordered geometry and accessibility",
            "viewport": [10, 20], "scale": 2, "pixels": [20, 40],
            "scenario": "exporter-owned scenario", "theme": "exporter-owned theme",
            "elements": [{"path": "same", "text": "first"}, {"path": "same", "text": "second"}],
            "accessibility": {},
        }
        for relative, (width, height, scale) in review.required_inventory().items():
            physical = [width * scale, height * scale]
            tree = {**self.tree, "viewport": [width, height], "scale": scale, "pixels": physical}
            self.frame(relative, tree=tree, physical=physical)
        self.subject = "inbox/phone-question-dark-320x568-1x.png"
        self.tree = json.loads((self.root / self.subject).with_suffix(".json").read_text())

    def frame(self, relative, tree=None, physical=None):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        width, height = self.tree["pixels"] if physical is None else physical
        # Deliberately header-only, including placeholder CRC. No IDAT or IEND.
        path.write_bytes(struct.pack(">8sI4sIIBBBBBI", review.PNG_SIGNATURE,
                                     13, b"IHDR", width, height, 8, 6, 0, 0, 0, 0))
        path.with_suffix(".json").write_text(json.dumps(self.tree if tree is None else tree))
        return path

    def produce(self):
        with patch.object(review, "source_metadata", return_value=copy.deepcopy(SOURCE)):
            return review.produce(self.root, self.root)

    def test_bundle_has_all_families_hashes_and_honest_status(self):
        manifest = self.produce()
        self.assertEqual(len(manifest["cases"]), 255)
        self.assertEqual({family: sum(case["family"] == family for case in manifest["cases"])
                          for family in review.FAMILIES},
                         {"inbox": 192, "settings": 34, "navigation": 28, "resources": 1})
        case = next(case for case in manifest["cases"] if case["png"] == self.subject)
        self.assertEqual(case["physical"], [320, 568])
        self.assertEqual(case["logical"], [320, 568])
        self.assertEqual(case["scenario"], "exporter-owned scenario")
        self.assertEqual(case["theme"], "exporter-owned theme")
        self.assertEqual(len(case["treeSha256"]), 64)
        self.assertEqual(manifest["reviewStatus"], "NOT REVIEWED")
        self.assertTrue(manifest["dirty"])
        self.assertIn("no image decoding", manifest["limits"])
        self.assertTrue((self.root / "judgments.md").read_text().startswith("NOT REVIEWED"))
        self.assertIn("Selected UX category", review.JUDGMENTS)
        self.assertIn("Independent fresh-context reviewer", review.JUDGMENTS)
        self.assertIn("8. Accessibility and contrast", review.JUDGMENTS)
        self.assertEqual(case["exporter"], "phone_inbox_visual_review_frames")
        self.assertEqual(review.exporter_for("inbox", "phone-acp-input-dark-320x280-1x"),
                         "phone_acp_visual_review_frames")
        self.assertEqual(review.exporter_for("settings", "phone-appearance-before"),
                         "mobile_appearance_and_real_theme_palette_render_accept_and_cancel")
        self.assertEqual(review.exporter_for("navigation", "retina-2x-native-false-640"),
                         "retina_navigation_renders_in_native_safe_area_and_desktop_phone_viewports")
        self.assertEqual(review.exporter_for("settings", "desktop"),
                         "settings_native_render_frames")
        self.assertEqual(review.exporter_for("navigation", "inventory-light-393"),
                         "session_browser_renders_realistic_dark_light_and_search_states")
        self.assertEqual(review.exporter_for("resources", "mixed"),
                         "native_png_headless_renderer_smoke")

    def test_missing_family_fails_without_writing_bundle(self):
        for png in (self.root / "inbox").glob("*.png"):
            png.unlink()
        with self.assertRaisesRegex(ValueError, "missing required cases: inbox/"):
            self.produce()
        self.assertFalse((self.root / "manifest.json").exists())

    def test_one_case_per_family_is_not_complete(self):
        for family in review.FAMILIES:
            for png in sorted((self.root / family).glob("*.png"))[1:]:
                png.unlink()
        with self.assertRaisesRegex(ValueError, "missing required cases"):
            self.produce()

    def test_each_small_light_keyboard_retina_scrolled_native_case_is_required(self):
        names = (
            "inbox/phone-question-dark-320x568-1x.png",
            "inbox/desktop-question-light-1100x720-1x.png",
            "inbox/phone-reply-dark-393x852-1x.png",
            "inbox/phone-reply-dark-393x852-2x.png",
            "inbox/desktop-scrolled-light-640x360-1x.png",
            "inbox/phone-native-reply-light-320x280-1x.png",
            "inbox/phone-sending-light-320x280-1x.png",
            "inbox/phone-other-question-dark-320x568-2x.png",
            "inbox/desktop-refused-light-640x360-1x.png",
            "settings/small-typed-search.png",
            "settings/small-no-match.png",
            "navigation/matching-light-320.png",
            "navigation/retina-2x-native-true-640.png",
        )
        for name in names:
            with self.subTest(case=name):
                png = self.root / name
                contents = png.read_bytes()
                png.unlink()
                with self.assertRaisesRegex(ValueError, "missing required cases"):
                    self.produce()
                png.write_bytes(contents)

    def test_missing_tree_fails(self):
        (self.root / self.subject).with_suffix(".json").unlink()
        with self.assertRaisesRegex(ValueError, "missing matching tree"):
            self.produce()

    def test_each_inline_keyboard_and_no_choices_case_is_required(self):
        names = (
            "phone-inline-dark-393x852-1x.png", "phone-inline-light-393x852-1x.png",
            "phone-inline-dark-320x568-1x.png", "phone-inline-light-320x568-1x.png",
            "phone-inline-no-choices-dark-393x852-1x.png",
            "phone-inline-no-choices-light-393x852-1x.png",
            "phone-inline-no-choices-dark-320x568-1x.png",
            "phone-inline-no-choices-light-320x568-1x.png",
        )
        for name in names:
            with self.subTest(case=name):
                png = self.root / "inbox" / name
                contents = png.read_bytes()
                png.unlink()
                with self.assertRaisesRegex(ValueError, "missing required cases"):
                    self.produce()
                png.write_bytes(contents)

    def test_pixel_mismatch_fails(self):
        tree = copy.deepcopy(self.tree)
        tree["pixels"] = [21, 40]
        self.frame(self.subject, tree=tree)
        with self.assertRaisesRegex(ValueError, "pixel dimensions differ"):
            self.produce()

    def test_each_acp_phone_and_native_reduced_case_is_required(self):
        names = [
            f"phone-acp-{state}-{theme}-{size}-1x.png"
            for state, theme, size in product(
                ("select", "ask", "form", "input"), ("dark", "light"),
                ("393x852", "320x568", "320x280"))
        ]
        self.assertEqual(len(names), 24)
        for name in names:
            with self.subTest(case=name):
                png = self.root / "inbox" / name
                contents = png.read_bytes()
                png.unlink()
                with self.assertRaisesRegex(ValueError, "missing required cases"):
                    self.produce()
                png.write_bytes(contents)

    def test_logical_rounding_is_bounded_to_half_a_pixel(self):
        tree = copy.deepcopy(self.tree)
        tree["viewport"] = [320.5, 568]
        self.frame("inbox/rounding.png", tree=tree)
        review.collect_cases(self.root)
        tree["viewport"] = [320.501, 568]
        self.frame("inbox/rounding.png", tree=tree)
        with self.assertRaisesRegex(ValueError, "viewport × scale"):
            self.produce()

    def test_invalid_headers_fail(self):
        png = self.root / self.subject
        png.write_bytes(b"not a png")
        with self.assertRaisesRegex(ValueError, "truncated PNG"):
            self.produce()

    def test_ordered_duplicate_paths_are_retained(self):
        manifest = self.produce()
        tree = json.loads((self.root / manifest["cases"][0]["tree"]).read_text())
        self.assertEqual(tree["elements"], self.tree["elements"])
        self.assertEqual(manifest["cases"][0]["elementCount"], 2)

    def test_mapping_or_empty_element_projection_fails(self):
        for elements in ({}, []):
            with self.subTest(elements=elements):
                tree = copy.deepcopy(self.tree)
                tree["elements"] = elements
                self.frame(self.subject, tree=tree)
                with self.assertRaises(ValueError):
                    self.produce()

    def test_nonpositive_or_nonfinite_geometry_fails(self):
        for scale in (0, -1, float("inf"), float("nan"), True):
            with self.subTest(scale=scale):
                tree = copy.deepcopy(self.tree)
                tree["scale"] = scale
                self.frame(self.subject, tree=tree)
                with self.assertRaisesRegex(ValueError, "scale must be positive"):
                    self.produce()

    def test_html_escapes_captions_and_encodes_relative_urls(self):
        tree = {**self.tree, "scenario": '<script>alert("scenario")</script>',
                "theme": 'light & "theme"'}
        self.frame('inbox/space & <tag> "quote" #?.png', tree=tree)
        self.produce()
        page = (self.root / "index.html").read_text()
        self.assertIn("space &amp; &lt;tag&gt;", page)
        self.assertIn("inbox/space%20%26%20%3Ctag%3E%20%22quote%22%20%23%3F.png", page)
        self.assertNotIn("<tag>", page)
        self.assertNotIn("<script>", page)
        self.assertIn("&lt;script&gt;", page)
        self.assertIn("light &amp; &quot;theme&quot;", page)
        self.assertNotIn(str(self.root), page)
        self.assertNotIn("https://", page)

    def test_symlink_escape_is_not_read(self):
        with tempfile.TemporaryDirectory() as outside:
            png = self.root / self.subject
            png.unlink()
            png.symlink_to(Path(outside) / "private.png")
            with self.assertRaises(ValueError):
                self.produce()

    def test_existing_output_files_are_never_overwritten(self):
        self.produce()
        before = (self.root / "manifest.json").read_bytes()
        with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
            self.produce()
        self.assertEqual((self.root / "manifest.json").read_bytes(), before)

    def test_required_case_rejects_internally_consistent_wrong_geometry(self):
        tree = copy.deepcopy(self.tree)
        tree["viewport"] = [160, 284]
        tree["scale"] = 2
        self.frame(self.subject, tree=tree)
        with self.assertRaisesRegex(ValueError, "geometry differs from maintained"):
            self.produce()

    def test_exporter_scenario_and_theme_must_be_nonempty_strings(self):
        for key in ("scenario", "theme"):
            for value in (None, "", "   ", 1):
                with self.subTest(key=key, value=value):
                    tree = copy.deepcopy(self.tree)
                    tree[key] = value
                    self.frame(self.subject, tree=tree)
                    with self.assertRaisesRegex(ValueError, f"exporter {key}"):
                        self.produce()
            self.frame(self.subject)

    def test_runner_rejects_relative_and_existing_paths_before_cargo(self):
        script = Path(__file__).with_name("visual-review.sh")
        for out in ("relative", str(self.root)):
            with self.subTest(out=out):
                result = subprocess.run(["bash", str(script), out], capture_output=True, text=True)
                self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "logs").exists())

    def test_source_metadata_includes_dirty_diff_and_untracked_source_hash(self):
        cargo = self.root / "clients/rune/Cargo.toml"
        cargo.parent.mkdir(parents=True)
        cargo.write_text('rune = { git = "https://example.invalid/rune", rev = "pin" }\n')
        source = self.root / "clients/rune/src/tests.rs"
        source.parent.mkdir()
        source.write_text("// untracked test source\n")
        replies = [
            b"revision\n", b"?? clients/rune/src/tests.rs\n", b"diff",
            b"clients/rune/src/tests.rs\0",
        ]
        with patch.object(review, "git", side_effect=replies):
            metadata = review.source_metadata(self.root)
        self.assertEqual(metadata["currentRunePin"], "pin")
        self.assertTrue(metadata["dirty"])
        self.assertEqual(metadata["untrackedSourceFiles"],
                         [{"path": "clients/rune/src/tests.rs", "sha256": review.digest(source)}])
        self.assertIn("not an exact", metadata["sourceQualification"])

    def test_runner_uses_serial_release_tests_sanitizes_env_and_packages(self):
        repo = self.root / "repo"
        scripts = repo / "clients/rune/scripts"
        scripts.mkdir(parents=True)
        for name in ("visual-review.sh", "visual_review.py"):
            shutil.copyfile(Path(__file__).with_name(name), scripts / name)
        (scripts.parent / "Cargo.toml").write_text('rune = { rev = "test-pin" }\n')
        for args in (["init", "-q"], ["add", "."],
                     ["-c", "core.hooksPath=/dev/null", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                      "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]):
            subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)
        binaries = self.root / "bin"
        binaries.mkdir()
        cargo = binaries / "cargo"
        cargo.write_text(f"#!{sys.executable}\n" + r'''
import json, os
from pathlib import Path
import struct, sys
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path.cwd() / "scripts"))
import visual_review as review
assert sys.argv[1:5] == ["test", "--release", "--locked", "--lib"]
assert sys.argv[6:] == ["--", "--nocapture", "--test-threads=1"]
assert not any(key.startswith(("PHUX_", "SIMCTL_CHILD_PHUX_")) for key in os.environ)
assert Path.cwd().name == "rune"
filters = {
    "phone_inbox_visual_review_frames": "RUNE_INBOX_SNAP",
    "phone_acp_visual_review_frames": "RUNE_INBOX_SNAP",
    "settings_native_render_frames": "RUNE_SETTINGS_SNAP",
    "mobile_appearance_and_real_theme_palette_render_accept_and_cancel": "RUNE_SETTINGS_SNAP",
    "session_browser_renders_realistic_dark_light_and_search_states": "RUNE_NAVIGATION_SNAP",
    "retina_navigation_renders_in_native_safe_area_and_desktop_phone_viewports": "RUNE_NAVIGATION_SNAP",
    "native_png_headless_renderer_smoke": "RUNE_RESOURCE_SNAP",
}
filter = sys.argv[5]
variable = filters[filter]
for relative, (width, height, scale) in review.required_inventory().items():
    case = Path(relative)
    if review.exporter_for(case.parts[0], case.stem) != filter:
        continue
    path = Path(os.environ[variable])
    if variable != "RUNE_RESOURCE_SNAP":
        path = path / case.name
    path.write_bytes(struct.pack(">8sI4sIIBBBBBI", b"\x89PNG\r\n\x1a\n",
                                13, b"IHDR", width * scale, height * scale, 8, 6, 0, 0, 0, 0))
    path.with_suffix(".json").write_text(json.dumps({
        "schema": 1, "projection": "test fixture", "viewport": [width, height],
        "scale": scale, "pixels": [width * scale, height * scale],
        "scenario": "exported fixture", "theme": "dark",
        "elements": [{"path": "root"}], "accessibility": {},
    }))
print("running 1 test")
print("test result: ok. 1 passed")
''')
        cargo.chmod(0o755)
        out = self.root / "fresh review"
        environment = {
            **os.environ, "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
            "PHUX_SOCKET": "do-not-connect", "PHUX_WS_TOKENS": "do-not-use",
            "SIMCTL_CHILD_PHUX_PROFILE": "default",
        }
        result = subprocess.run(["bash", str(scripts / "visual-review.sh"), str(out)],
                                env=environment, cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        manifest = json.loads((out / "manifest.json").read_text())
        self.assertEqual(len(manifest["cases"]), 255)
        self.assertEqual(manifest["currentRunePin"], "test-pin")
        self.assertFalse(manifest["dirty"])
        self.assertEqual(len(list((out / "logs").glob("*.log"))), 8)


if __name__ == "__main__":
    unittest.main()
