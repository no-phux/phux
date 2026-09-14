#!/usr/bin/env python3
"""Refuse the Native SDK terminal store for Cockpit product panes.

The SDK is shell only (windows, chrome, event loop, gpu_surface,
canvas.terminal_grid.paint). libghostty-vt Session in src/terminal/ is the
engine. Product panes must not use runtime/terminal_session.zig or the
<terminal pty=> markup widget. local.max_live_shells must stay derived from
native_sdk.max_effect_ptys. See docs/DECISIONS.md.
"""

from pathlib import Path
import re
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src"
PROVIDER = SRC / "providers" / "local" / "provider.zig"
SUFFIXES = {".native", ".ts", ".zig"}

WIDGET = re.compile(r"<\s*terminal\b[^>]*\bpty\s*=", re.IGNORECASE | re.DOTALL)
STORE = re.compile(r"\bterminal_session\b")
DERIVED_SHELLS = re.compile(
    r"pub const max_live_shells:\s*usize\s*=\s*native_sdk\.max_effect_ptys\s*;"
)
LITERAL_SHELLS = re.compile(r"pub const max_live_shells:\s*usize\s*=\s*\d+")


def strip_comments(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.DOTALL)
    text = re.sub(r"<!--.*?-->", " ", text, flags=re.DOTALL)
    return re.sub(r"//.*?$", " ", text, flags=re.MULTILINE)


def product_files(directory):
    for path in sorted(directory.rglob("*")):
        if path.is_file() and path.suffix in SUFFIXES:
            yield path


def scan(directory):
    hits = []
    for path in product_files(directory):
        code = strip_comments(path.read_text(encoding="utf-8"))
        rel = path.relative_to(directory).as_posix()
        if WIDGET.search(code):
            hits.append(f"{rel}: framework <terminal pty=> widget")
        if STORE.search(code):
            hits.append(f"{rel}: framework terminal_session store")
    return hits


class ShellEngine(unittest.TestCase):
    def test_product_source_refuses_framework_terminal_store(self):
        self.assertEqual(scan(SRC), [])

    def test_max_live_shells_is_derived(self):
        source = PROVIDER.read_text(encoding="utf-8")
        self.assertRegex(source, DERIVED_SHELLS)
        self.assertIsNone(LITERAL_SHELLS.search(source))

    def test_widget_reintroduction_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            pane = Path(directory) / "windows" / "pane.native"
            pane.parent.mkdir()
            pane.write_text("<column>\n  <terminal\n    pty={key}\n  />\n</column>\n")
            self.assertIn("framework <terminal pty=> widget", "\n".join(scan(Path(directory))))

    def test_store_reintroduction_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "runtime.zig"
            path.write_text('const store = native_sdk.runtime.terminal_session;\n')
            self.assertIn("framework terminal_session store", "\n".join(scan(Path(directory))))

    def test_refusal_comments_are_allowed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.zig"
            path.write_text(
                "//! Refuse runtime/terminal_session.zig and `<terminal pty=>`.\n"
                "const std = @import(\"std\");\n"
            )
            self.assertEqual(scan(Path(directory)), [])


if __name__ == "__main__":
    unittest.main()
