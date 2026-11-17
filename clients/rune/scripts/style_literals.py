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
    "const": r"const\s+\w+\s*:\s*(?:f32|TextStyle|Color)\b",
}
COMPILED = {name: re.compile(p) for name, p in PATTERNS.items()}

# Where literals are legal: the token definitions.
OWNERS = ("src/ui/tokens.rs", "src/ui/tokens/", "src/theme.rs", "src/theme/")
# Illustration art: scenes, their painters and the onboarding windows.
SCENES = ("pitch", "reef", "hive", "kitchen", "galaxy", "pitlane")
ART = tuple(
    [f"src/{s}.rs" for s in SCENES]
    + [f"src/{s}/" for s in SCENES]
    + [f"src/ui/{s}/" for s in SCENES]
    + ["src/ui/playground/pen.rs", "src/ui/onboarding/look.rs"]
)


def exempt(rel, text):
    return rel.startswith(OWNERS) or rel.startswith(ART) or HEADER in text[:2000]


def strip_comments(text):
    """Drop whole-line comments, where examples and citations live."""
    return "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("//"))


def count(text):
    body = strip_comments(text)
    return sum(len(rx.findall(body)) for rx in COMPILED.values())


def scan(root=ROOT):
    counts = {}
    for path in sorted((root / "src").rglob("*.rs")):
        rel = path.relative_to(root).as_posix()
        text = path.read_text(encoding="utf-8")
        if exempt(rel, text):
            continue
        n = count(text)
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
            n = 0 if exempt(rel, text) else count(text)
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
