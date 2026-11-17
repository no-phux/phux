#!/usr/bin/env python3
"""The Rune client's raw-style-literal ratchet (docs/design-language.md).

Views style with tokens (src/ui/tokens.rs). This counts the raw literals
that bypass them -- radii, blur, opacity, springs, spacing and sizes, type,
borders, colours, and local style constants -- per file, outside the owner
paths (where tokens are defined) and the illustration art.

The committed baseline (scripts/style_literals.baseline.json) maps each file
to its count. The check fails when a file's count rises or a file with
literals appears that the baseline does not list. Counts may fall freely;
lanes never edit the baseline, and the final cleanup rewrites it.

Usage:
    python3 scripts/style_literals.py                 # check against the baseline
    python3 scripts/style_literals.py --files a.rs    # print counts for files
    python3 scripts/style_literals.py --write-baseline
"""

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASELINE = ROOT / "scripts" / "style_literals.baseline.json"
HEADER = "style-literals: illustration"

PATTERNS = {
    "radius": r"\.radius\(\s*[0-9]",
    "blur": r"\.backdrop_blur\(\s*[0-9]",
    "opacity": r"\.(?:opacity|dim|layer_opacity|group_opacity|opacity_motion)\(\s*0?\.[0-9]",
    "spring": r"Spring::new\(",
    "scale": r"Transition::scale\(\s*0?\.[0-9]",
    "space": r"\.(?:p|px|py|pt|pb|pl|pr|padding|m|mx|my|mt|mb|ml|mr|gap|w|h|min_w|min_h|max_w|max_h|size)\(\s*[1-9][0-9.]*",
    "type": r"\.(?:lh|tracking|weight)\(\s*[0-9]",
    "type_struct": r"TextStyle\s*\{[^}]*size\s*:",
    "border": r"\.border\(\s*[0-9]",
    "hex": r"0x[0-9a-fA-F]{6}\b",
    "color_struct": r"Color\s*\{\s*r\s*:",
    # A local style constant with a literal value: `const R: f32 = 7.0;`
    # then `.radius(R)`. A constant naming a token (`= SPACE.s3`) is fine.
    "const": r"const\s+\w+\s*:\s*(?:f32|TextStyle|Color)\s*=\s*(?:-?[0-9]|TextStyle::new|Color\s*\{)",
    # A colour or type role rebuilt by struct update (`Color { a: x, ..c }`,
    # `TextStyle { weight: w, ..role }`) skips `Tokens::alpha` and the
    # `Voice` variants.
    "color_update": r"Color\s*\{[^{}]*\.\.",
    "type_update": r"TextStyle\s*\{(?![^{}]*size\s*:)[^{}]*\.\.",
    # Geometry as a multiple of a spacing token (`s.s4 * 13.0`): a layout
    # value under another name.
    "space_multiple": r"\b(?:s|space)\.\w+\s*\*\s*[0-9]",
}
COMPILED = {name: re.compile(p) for name, p in PATTERNS.items()}

# A view's own geometry table: a `const` struct literal. Each numeric field
# counts as a literal.
CONST_STRUCT = re.compile(r"\bconst\s+\w+\s*:\s*[A-Z]\w*\s*=\s*[A-Z]\w*\s*\{")
NUMERIC_FIELD = re.compile(r"\b\w+\s*:\s*-?[0-9][0-9_.]*(?:f32)?\s*[,}]")
# The forms counted only in a view, where a local table or update is style.
VIEW_ONLY = ("const", "color_update", "type_update", "space_multiple")

# Local style constants are a view's bypass; elsewhere (gesture slop, font
# size limits, timing) a constant is not style.
VIEWS = ("src/ui.rs", "src/ui/", "src/features/")

# Where literals are legal: the token definitions, and the terminal's own
# drawing (`src/grid*`: cell geometry, the cursor and SGR underlines are VT
# content, not chrome).
OWNERS = ("src/ui/tokens.rs", "src/ui/tokens/", "src/theme.rs", "src/theme/", "src/grid.rs", "src/grid/")
# Illustration art: scenes, their painters and the onboarding windows.
SCENES = ("pitch", "reef", "hive", "kitchen", "galaxy", "pitlane")
ART = tuple(
    [f"src/{s}.rs" for s in SCENES]
    + [f"src/{s}/" for s in SCENES]
    + [f"src/ui/{s}/" for s in SCENES]
    + ["src/ui/playground/pen.rs", "src/ui/onboarding/look.rs"]
)


def is_test(rel):
    """Test fixtures lay out at whatever size the case needs."""
    *dirs, name = rel.split("/")
    in_tests = any(d == "tests" or d.endswith("_tests") for d in dirs)
    return in_tests or name in ("tests.rs", "test_support.rs") or name.endswith("_tests.rs")


def exempt(rel, text):
    return rel.startswith(OWNERS) or rel.startswith(ART) or is_test(rel) or HEADER in text[:2000]


def strip_comments(text):
    """Drop whole-line comments, where examples and citations live."""
    return "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("//"))


def const_tables(body):
    """Numeric fields in a view's `const` struct literals."""
    n = 0
    for m in CONST_STRUCT.finditer(body):
        depth, i = 1, m.end()
        while depth and i < len(body):
            depth += {"{": 1, "}": -1}.get(body[i], 0)
            i += 1
        n += len(NUMERIC_FIELD.findall(body[m.end():i]))
    return n


def count(text, rel=None):
    """Raw literals in `text`; local style constants, tables and struct
    updates only in a view (`rel` of None counts as one)."""
    body = strip_comments(text)
    view = rel is None or rel.startswith(VIEWS)
    n = sum(len(rx.findall(body)) for name, rx in COMPILED.items() if view or name not in VIEW_ONLY)
    return n + (const_tables(body) if view else 0)


def scan(root=ROOT):
    counts = {}
    for path in sorted((root / "src").rglob("*.rs")):
        rel = path.relative_to(root).as_posix()
        text = path.read_text(encoding="utf-8")
        if exempt(rel, text):
            continue
        n = count(text, rel)
        if n:
            counts[rel] = n
    return counts


def check(counts, baseline):
    """Every file that rose or is new, with its counts."""
    problems = []
    for rel, n in sorted(counts.items()):
        was = baseline.get(rel)
        if was is None:
            problems.append(f"{rel}: {n} raw style literal(s) in a file the baseline does not list")
        elif n > was:
            problems.append(f"{rel}: {n} raw style literals, up from {was}")
    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--files", nargs="+", help="print the count for each file (0 for a converted one)")
    parser.add_argument("--write-baseline", action="store_true", help="record today's counts")
    args = parser.parse_args(argv)
    if args.files:
        total = 0
        for f in args.files:
            path = Path(f) if Path(f).is_absolute() else ROOT / f
            rel = path.resolve().relative_to(ROOT).as_posix()
            text = path.read_text(encoding="utf-8")
            n = 0 if exempt(rel, text) else count(text, rel)
            total += n
            print(f"{n}\t{rel}")
        print(f"{total}\ttotal")
        return 0
    counts = scan()
    if args.write_baseline:
        BASELINE.write_text(json.dumps(counts, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"style literals: baseline written, {sum(counts.values())} in {len(counts)} files")
        return 0
    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    problems = check(counts, baseline)
    for p in problems:
        print(f"style literals: {p}", file=sys.stderr)
    if problems:
        print("style literals: use tokens (src/ui/tokens.rs); see docs/design-language.md", file=sys.stderr)
        return 1
    print(f"style literals: ok, {sum(counts.values())} left in {len(counts)} files (baseline {sum(baseline.values())})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
