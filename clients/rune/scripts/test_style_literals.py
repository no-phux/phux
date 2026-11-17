"""The raw-style-literal ratchet, and the clippy config it pairs with."""

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import style_literals  # noqa: E402

RUNE = Path(__file__).resolve().parents[1]
REPO = RUNE.parents[1]


def toml_keys(path):
    """The top-level `key = value` lines of a small TOML file."""
    keys = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if "=" in line and not line.startswith(("[", "{")):
            key, value = (part.strip() for part in line.split("=", 1))
            if key.replace("-", "").isalnum():
                keys[key] = value
    return keys


class Patterns(unittest.TestCase):
    def test_each_form_counts(self):
        for snippet in [
            ".radius(7.0)",
            ".backdrop_blur(18.0)",
            ".opacity(0.5)",
            ".layer_opacity(.4)",
            "Spring::new(0.2, 0.0)",
            "Transition::scale(0.97)",
            ".px(14.0)",
            ".size(26.0, 26.0)",
            ".lh(16.0)",
            "TextStyle { size: 12.0, ..k.ty.ui }",
            ".border(1.0, c)",
            "0x1e1f22",
            "Color { r: 0.1, g: 0.2, b: 0.3, a: 1.0 }",
            "const R: f32 = 7.0;",
            ".fill(Color { a: alpha, ..tint })",
            "TextStyle { weight: 600.0, ..t.ty.label }",
            "TextStyle { family: MONO, ..t.ty.caption }",
            "s.s4 * 13.0",
            "t.space.s16 * 3.0",
            "const G: Geometry = Geometry { sheet_w: 420.0 };",
        ]:
            self.assertEqual(style_literals.count(snippet), 1, snippet)

    def test_a_view_table_counts_each_numeric_field(self):
        table = "const G: Geometry = Geometry {\n    a: 1.0,\n    b: SPACE.s3,\n    c: 0.8,\n};"
        self.assertEqual(style_literals.count(table), 2)
        self.assertEqual(style_literals.count(table, "src/desk/touch.rs"), 0)

    def test_struct_updates_count_only_in_a_view(self):
        update = "Color { a: alpha, ..tint }"
        self.assertEqual(style_literals.count(update, "src/ui/sidebar.rs"), 1)
        self.assertEqual(style_literals.count(update, "src/grid/cursor.rs"), 0)

    def test_tokens_and_zero_do_not_count(self):
        for snippet in [
            ".radius(t.radius.sm)",
            ".px(t.space.row_inset_x)",
            ".min_w(0.0)",
            ".w(0.0)",
            "// .radius(7.0) in a comment",
            "const MAX: usize = 4;",
            "const GAP: f32 = SPACE.s3;",
            "t.alpha(tint, alpha)",
            "t.ty.label.semibold()",
            "s.touch_min / 2.0",
        ]:
            self.assertEqual(style_literals.count(snippet), 0, snippet)

    def test_a_constant_outside_a_view_is_not_style(self):
        self.assertEqual(style_literals.count("const SLOP: f32 = 8.0;", "src/desk/touch.rs"), 0)
        self.assertEqual(style_literals.count("const R: f32 = 7.0;", "src/ui/sidebar.rs"), 1)
        self.assertEqual(style_literals.count(".radius(7.0)", "src/desk/touch.rs"), 1)

    def test_owner_and_art_paths_are_exempt(self):
        self.assertTrue(style_literals.exempt("src/ui/tokens.rs", ""))
        self.assertTrue(style_literals.exempt("src/ui/tokens/kit.rs", ""))
        self.assertTrue(style_literals.exempt("src/theme/builtin.rs", ""))
        self.assertTrue(style_literals.exempt("src/pitch/ground.rs", ""))
        self.assertTrue(style_literals.exempt("src/ui/galaxy/sky.rs", ""))
        self.assertTrue(style_literals.exempt("src/ui/x.rs", "//! style-literals: illustration\n"))
        self.assertTrue(style_literals.exempt("src/grid.rs", ""))
        self.assertTrue(style_literals.exempt("src/ui/sidebar/tests.rs", ""))
        self.assertTrue(style_literals.exempt("src/ui/gallery_tests/fixture.rs", ""))
        self.assertFalse(style_literals.exempt("src/ui/sidebar.rs", ""))

    def test_the_check_refuses_growth_and_new_files(self):
        base = {"src/a.rs": 3}
        self.assertEqual(style_literals.check({"src/a.rs": 2}, base), [])
        self.assertEqual(len(style_literals.check({"src/a.rs": 4}, base)), 1)
        self.assertEqual(len(style_literals.check({"src/b.rs": 1}, base)), 1)


class Tree(unittest.TestCase):
    def test_the_baseline_is_empty(self):
        # §5.1's end state: no raw style literal outside owner and art paths.
        baseline = json.loads(style_literals.BASELINE.read_text(encoding="utf-8"))
        self.assertEqual(baseline, {})

    def test_the_tree_is_within_its_baseline(self):
        baseline = json.loads(style_literals.BASELINE.read_text(encoding="utf-8"))
        self.assertEqual(style_literals.check(style_literals.scan(), baseline), [])

    def test_the_rune_clippy_config_copies_the_roots_keys(self):
        # Clippy reads the nearest clippy.toml and does not merge, so the
        # client's copy must say what the root's does.
        root = toml_keys(REPO / "clippy.toml")
        rune = toml_keys(RUNE / "clippy.toml")
        self.assertTrue(root)
        for key, value in root.items():
            self.assertEqual(rune.get(key), value, key)


if __name__ == "__main__":
    unittest.main()
