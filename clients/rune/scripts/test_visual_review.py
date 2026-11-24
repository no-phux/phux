"""Header-only fixtures qualify the packager, never an actual image decoder."""
import copy
import json
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
SUBJECT = "inbox/phone-question-dark-320x568-1x.png"


def motion_fixture(path, tree):
    if not path.stem.startswith(("pane-motion-", "accessory-resize-")):
        return
    frames = review.motion_frames(path.stem)
    stage = next(stage for stage in frames if path.stem.endswith(f"-{stage}"))
    scenario = path.stem[:-(len(stage) + 1)]
    tree["scenario"] = scenario
    frame = frames[stage]
    path.with_suffix(".motion.json").write_text(json.dumps({
        "schema": 1, "scenario": scenario, "stage": stage,
        "frame": frame, "elapsed_ms": frame * 1000.0 / 60.0,
    }))


class VisualReview(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.tree = {
            "schema": 1, "projection": "ordered geometry and accessibility",
            "viewport": [320, 568], "scale": 1, "pixels": [320, 568],
            "scenario": "exporter-owned scenario", "theme": "exporter-owned theme",
            "elements": [{"path": "same", "text": "first"}, {"path": "same", "text": "second"}],
            "accessibility": {},
        }
        self.subject = SUBJECT
        self.frame(self.subject)
        # Most tests exercise one malformed case or the bundle writer, not
        # the full maintained inventory. Inventory tests opt back into the real set.
        self.inventory = patch.object(review, "required_inventory",
                                      return_value={SUBJECT: (320, 568, 1)})
        self.inventory.start()
        self.addCleanup(self.inventory.stop)

    def populate(self, inventory):
        for relative, (width, height, scale) in inventory.items():
            physical = [round(width * scale), round(height * scale)]
            tree = {**self.tree, "viewport": [width, height], "scale": scale, "pixels": physical}
            self.frame(relative, tree=tree, physical=physical)

    def frame(self, relative, tree=None, physical=None):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        width, height = self.tree["pixels"] if physical is None else physical
        # Deliberately header-only, including placeholder CRC. No IDAT or IEND.
        path.write_bytes(struct.pack(">8sI4sIIBBBBBI", review.PNG_SIGNATURE,
                                     13, b"IHDR", width, height, 8, 6, 0, 0, 0, 0))
        tree = copy.deepcopy(self.tree if tree is None else tree)
        motion_fixture(path, tree)
        path.with_suffix(".json").write_text(json.dumps(tree))
        return path

    def produce(self):
        with patch.object(review, "source_metadata", return_value=copy.deepcopy(SOURCE)):
            return review.produce(self.root, self.root)

    def test_bundle_has_all_families_hashes_and_honest_status(self):
        self.inventory.stop()
        self.populate(review.required_inventory())
        manifest = self.produce()
        self.assertEqual(len(manifest["cases"]), 1497)
        self.assertEqual({family: sum(case["family"] == family for case in manifest["cases"])
                          for family in review.FAMILIES},
                            {"inbox": 404, "settings": 226, "navigation": 866, "resources": 1})
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
        self.assertEqual(review.exporter_for(
            "navigation", "sidebar-focus-dark-normal-row-1100x720-2x"),
            "sidebar_focus_visual_review_frames")
        self.assertEqual(review.exporter_for("navigation", "header-split-dark-native-true-2x-640"),
                          "phone_header_visual_review_frames")
        self.assertEqual(review.exporter_for("navigation", "states-slow-dark-native-true-2x-786"),
                         "phone_states_visual_review_frames")
        self.assertEqual(review.exporter_for("navigation", "pane-motion-horizontal-dark"),
                         "phone_pane_motion_visual_review_frames")
        self.assertEqual(review.exporter_for("navigation", "connections-large-light-2x-native-true-bottom-640"),
                         "phone_connections_visual_review_frames")
        self.assertEqual(review.exporter_for("navigation", "connections-keyboard-dark-bottom-640"),
                         "phone_connections_keyboard_visual_review_frames")
        self.assertEqual(review.exporter_for("settings", "desktop"),
                         "settings_native_render_frames")
        self.assertEqual(review.exporter_for("settings", "size-small-dark-ax5-text-bigger-1x"),
                         "settings_size_visual_review_frames")
        self.assertEqual(review.exporter_for("settings", "theme-native-small-light-ax5-revealed-2x"),
                         "settings_theme_visual_review_frames")
        self.assertEqual(review.exporter_for("navigation", "inventory-light-393"),
                         "session_browser_renders_realistic_dark_light_and_search_states")
        self.assertEqual(review.exporter_for("resources", "mixed"),
                          "native_png_headless_renderer_smoke")

    def test_reconnect_matrix_keeps_constrained_native_and_desktop_cases(self):
        self.inventory.stop()
        inventory = {path: geometry for path, geometry in review.required_inventory().items()
                     if path.startswith("navigation/reconnect-")}
        self.assertEqual(len(inventory), 224)
        for path, geometry in inventory.items():
            self.assertEqual(review.exporter_for("navigation", Path(path).stem),
                             "reconnect_feedback_visual_review_frames")
            if "-native-true-" in path:
                self.assertIn(geometry, ((320, 340, 2), (393, 480, 2)))
            elif "-phone-false-" in path:
                width = 1100 if path.endswith("-1100.png") else 640
                height = 720 if width == 1100 else 360
                self.assertAlmostEqual(geometry[0] * geometry[2], width, places=3)
                self.assertAlmostEqual(geometry[1] * geometry[2], height, places=3)
                self.assertAlmostEqual(geometry[2], 1.4 if "-large-true-" in path else 1)
                if "-large-true-" in path:
                    with self.assertRaisesRegex(ValueError, "geometry differs"):
                        review.validate_required_geometry(
                            {"png": path, "logical": [width, height], "scale": 1}, geometry)

    def test_missing_family_fails_without_writing_bundle(self):
        self.inventory.stop()
        (self.root / self.subject).unlink()
        self.populate({path: size for path, size in review.required_inventory().items()
                       if not path.startswith("inbox/")})
        with self.assertRaisesRegex(ValueError, "missing required cases: inbox/"):
            self.produce()
        self.assertFalse((self.root / "manifest.json").exists())

    def test_selected_acp_forms_are_required_and_keep_existing_acp_cases(self):
        self.inventory.stop()
        self.populate(review.required_inventory())
        selected = review.form_inventory(review.THEMES)
        self.assertEqual(len(selected), 152)
        self.assertTrue(selected.keys() <= review.inbox_inventory().keys())
        for path in selected:
            self.assertEqual(review.exporter_for("inbox", Path(path).stem),
                             "acp_form_visual_review_frames")
        native = "inbox/acp-form-native-small-long-ax5-dark-"
        self.assertEqual(selected[native + "top-320x478-2x.png"], (320, 478, 2))
        self.assertEqual(selected[native + "field-focus-320x340-2x.png"], (320, 340, 2))
        self.assertEqual(selected[native + "field-heading-320x340-2x.png"], (320, 340, 2))
        old_acp = {path for path in review.inbox_inventory() if "/phone-acp-" in path}
        self.assertEqual(len(old_acp), 24)
        subject = native + "final-action-320x340-2x.png"
        (self.root / subject).unlink()
        with self.assertRaisesRegex(ValueError, "missing required cases"):
            self.produce()

    def test_supported_ask_owner_flow_is_required(self):
        self.inventory.stop()
        selected = review.ask_owner_inventory(review.THEMES)
        self.assertEqual(len(selected), 60)
        self.assertTrue(selected.keys() <= review.inbox_inventory().keys())
        for path in selected:
            self.assertEqual(review.exporter_for("inbox", Path(path).stem),
                             "acp_ask_owner_visual_review_frames")
        subject = "inbox/acp-ask-owner-native-small-dark-b-open-320x340-2x.png"
        self.assertEqual(selected[subject], (320, 340, 2))
        self.populate(review.required_inventory())
        (self.root / subject).unlink()
        with self.assertRaisesRegex(ValueError, "missing required cases"):
            self.produce()

    def test_one_case_per_family_is_not_complete(self):
        self.inventory.stop()
        inventory = review.required_inventory()
        representatives = {}
        for family in review.FAMILIES:
            path = next(path for path in inventory if path.startswith(family + "/"))
            representatives[path] = inventory[path]
        (self.root / self.subject).unlink()
        self.populate(representatives)
        with self.assertRaisesRegex(ValueError, "missing required cases"):
            self.produce()

    def test_missing_tree_fails(self):
        (self.root / self.subject).with_suffix(".json").unlink()
        with self.assertRaisesRegex(ValueError, "missing matching tree"):
            self.produce()

    def test_pixel_mismatch_fails(self):
        tree = copy.deepcopy(self.tree)
        tree["pixels"] = [21, 40]
        self.frame(self.subject, tree=tree)
        with self.assertRaisesRegex(ValueError, "pixel dimensions differ"):
            self.produce()

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
        for name in ("visual-review.sh", "visual_review.py", "visual_motion.py",
                     "visual_navigation.py", "visual_states.py", "visual_themes.py", "visual_acp.py"):
            shutil.copyfile(Path(__file__).with_name(name), scripts / name)
        (scripts.parent / "Cargo.toml").write_text('rune = { rev = "test-pin" }\n')
        for args in (["init", "-q"], ["add", "."],
                     ["-c", "core.hooksPath=/dev/null", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                      "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]):
            subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)
        binaries = self.root / "bin"
        binaries.mkdir()
        cargo = binaries / "cargo"
        fixture = Path(__file__).with_name("visual_runner_fixture.py").read_text()
        cargo.write_text(f"#!{sys.executable}\n{fixture}")
        cargo.chmod(0o755)
        out = self.root / "fresh review"
        private_seed = self.root / "inherited-private-prefs"
        seed = private_seed / "sh.phux.rune/prefs.json"
        seed.parent.mkdir(parents=True)
        seed_bytes = b'{"font_size":15,"tour_offered":false}\n'
        seed.write_bytes(seed_bytes)
        environment = {
            **os.environ, "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
            "PHUX_SOCKET": "do-not-connect", "PHUX_WS_TOKENS": "do-not-use",
            "SIMCTL_CHILD_PHUX_PROFILE": "default",
            "RUNE_PREFS_DIR": str(private_seed),
        }
        result = subprocess.run(["bash", str(scripts / "visual-review.sh"), str(out)],
                                env=environment, cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        manifest = json.loads((out / "manifest.json").read_text())
        self.assertEqual(len(manifest["cases"]), 1497)
        self.assertEqual(manifest["currentRunePin"], "test-pin")
        self.assertFalse(manifest["dirty"])
        self.assertEqual(len(list((out / "logs").glob("*.log"))), 22)
        self.assertEqual(seed.read_bytes(), seed_bytes)
        self.assertEqual({path.relative_to(private_seed).as_posix(): path.read_bytes()
                          for path in private_seed.rglob("*") if path.is_file()},
                         {"sh.phux.rune/prefs.json": seed_bytes})
        self.assertEqual(json.loads((out / "prefs/runner-fixture.json").read_text()),
                         {"fixture": True})


if __name__ == "__main__":
    unittest.main()
