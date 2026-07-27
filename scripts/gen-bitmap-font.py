#!/usr/bin/env python3
"""Convert the vendored Spleen BDF face into a Rust const glyph table.

This script is *reproducibility documentation*, not a build step. There is no
build.rs anywhere in this workspace, and there is deliberately none here: the
generated file is committed, so a normal `cargo build` compiles a plain Rust
source file with no Python, no BDF parser, and no codegen in the dependency
graph. Run this only when the vendored face is upgraded, then commit the
regenerated .rs alongside the new .bdf.

    scripts/gen-bitmap-font.py \\
        crates/phux-record/assets/spleen-8x16.bdf \\
        crates/phux-record/src/font/spleen_8x16.rs

Output shape, matching `phux_record::font::BitmapFont::ranges`: a sorted,
non-overlapping table of `(start_codepoint, end_codepoint, &[[u8; 16]])`
groups. Each glyph is 16 bytes, one per pixel row, most significant bit at the
leftmost of the eight columns -- the same orientation the BDF BITMAP block
already uses for an 8-pixel-wide face, so the conversion is a straight
hex-parse with no bit reversal.

The BDF is parsed rather than trusted blindly: a face whose FONTBOUNDINGBOX is
not 8x16, or a glyph whose BBX disagrees with it, is a hard error. Silently
accepting a differently-shaped face would produce a table that compiles and
renders garbage, which is far worse than refusing to generate.
"""

from __future__ import annotations

import sys
from pathlib import Path

CELL_W = 8
CELL_H = 16

# Ranges wider than this many missing codepoints are split rather than padded
# with blanks. Padding a handful of holes keeps the range table short (fewer
# binary-search steps, less source noise); padding a 60k-codepoint hole between
# Latin and the Private Use Area would embed megabytes of zeroes.
MAX_GAP = 8


def parse_bdf(text: str) -> dict[int, list[int]]:
    """Return `{codepoint: [16 row bytes]}` for every glyph in the face."""
    glyphs: dict[int, list[int]] = {}
    encoding: int | None = None
    bbx: tuple[int, int, int, int] | None = None
    rows: list[int] | None = None
    bounding: tuple[int, int] | None = None

    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if rows is not None:
            if line == "ENDCHAR":
                if encoding is None or bbx is None:
                    raise SystemExit(f"{lineno}: ENDCHAR with no ENCODING/BBX")
                if (bbx[0], bbx[1]) != (CELL_W, CELL_H):
                    raise SystemExit(
                        f"{lineno}: glyph U+{encoding:04X} has BBX "
                        f"{bbx[0]}x{bbx[1]}, expected {CELL_W}x{CELL_H}"
                    )
                if len(rows) != CELL_H:
                    raise SystemExit(
                        f"{lineno}: glyph U+{encoding:04X} has {len(rows)} "
                        f"bitmap rows, expected {CELL_H}"
                    )
                # ENCODING -1 marks an unencoded glyph; it has no codepoint to
                # look up by, so it cannot be reached and is dropped.
                if encoding >= 0:
                    glyphs[encoding] = rows
                encoding, bbx, rows = None, None, None
                continue
            # An 8-wide glyph is exactly one hex byte per row. Wider faces pad
            # to a byte boundary; we reject those in the BBX check above, so
            # anything else here is a malformed file.
            if len(line) != 2:
                raise SystemExit(f"{lineno}: expected one hex byte, got {line!r}")
            rows.append(int(line, 16))
            continue

        if line.startswith("FONTBOUNDINGBOX "):
            parts = line.split()
            bounding = (int(parts[1]), int(parts[2]))
        elif line.startswith("ENCODING "):
            encoding = int(line.split()[1])
        elif line.startswith("BBX "):
            parts = line.split()
            bbx = (int(parts[1]), int(parts[2]), int(parts[3]), int(parts[4]))
        elif line == "BITMAP":
            rows = []

    if bounding != (CELL_W, CELL_H):
        raise SystemExit(f"FONTBOUNDINGBOX is {bounding}, expected ({CELL_W}, {CELL_H})")
    if not glyphs:
        raise SystemExit("no encoded glyphs found")
    return glyphs


def group(codepoints: list[int]) -> list[tuple[int, int]]:
    """Collapse sorted codepoints into `(start, end)` runs, bridging small gaps."""
    runs: list[tuple[int, int]] = []
    start = prev = codepoints[0]
    for code in codepoints[1:]:
        if code - prev - 1 <= MAX_GAP:
            prev = code
            continue
        runs.append((start, prev))
        start = prev = code
    runs.append((start, prev))
    return runs


def emit(glyphs: dict[int, list[int]], source: Path) -> str:
    codepoints = sorted(glyphs)
    runs = group(codepoints)
    blank = [0] * CELL_H

    out: list[str] = []
    out.append("//! Spleen 8x16 glyph bitmaps, generated -- do not edit by hand.")
    out.append("//!")
    out.append("//! Source: Spleen 2.1.0, <https://www.cambus.net/spleen-monospaced-bitmap-fonts/>,")
    out.append("//! vendored verbatim at `crates/phux-record/assets/spleen-8x16.bdf`.")
    out.append("//! Copyright (c) 2018-2024, Frederic Cambus. Licensed BSD-2-Clause, whose")
    out.append("//! redistribution clause requires the copyright notice and disclaimer to travel")
    out.append("//! with both source and binary forms of phux; that notice lives verbatim at")
    out.append("//! `/LICENSE-SPLEEN` in the repository root. Do not delete that file, and do not")
    out.append("//! swap this face for an OFL-1.1 one -- OFL-1.1 is not on deny.toml's allow list.")
    out.append("//!")
    out.append("//! Regenerate with `scripts/gen-bitmap-font.py` after upgrading the .bdf. The")
    out.append("//! script is documentation of how this file came to be, never a build step:")
    out.append("//! this file is committed and compiled as ordinary Rust.")
    out.append("//!")
    out.append("//! One byte per pixel row, most significant bit at the leftmost of the eight")
    out.append(f"//! columns. {len(codepoints)} glyphs in {len(runs)} contiguous ranges.")
    out.append("")

    names: list[str] = []
    for start, end in runs:
        name = f"R_{start:04X}_{end:04X}"
        names.append(name)
        span = end - start + 1
        out.append("#[rustfmt::skip]")
        out.append(f"static {name}: [[u8; {CELL_H}]; {span}] = [")
        for code in range(start, end + 1):
            # Bridged gaps get an all-zero glyph: it renders as a blank cell,
            # which is the right answer for a codepoint the face never had.
            body = ",".join(f"0x{b:02x}" for b in glyphs.get(code, blank))
            out.append(f"    [{body}], // U+{code:04X}")
        out.append("];")
        out.append("")

    out.append("/// Sorted, non-overlapping `(start, end, bitmaps)` groups.")
    out.append("#[rustfmt::skip]")
    out.append(f"pub(super) static RANGES: [(u32, u32, &[[u8; {CELL_H}]]); {len(runs)}] = [")
    for (start, end), name in zip(runs, names):
        out.append(f"    (0x{start:04X}, 0x{end:04X}, &{name}),")
    out.append("];")
    out.append("")
    return "\n".join(out)


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(f"usage: {argv[0]} <in.bdf> <out.rs>", file=sys.stderr)
        return 2
    src, dst = Path(argv[1]), Path(argv[2])
    glyphs = parse_bdf(src.read_text(encoding="utf-8", errors="replace"))
    dst.write_text(emit(glyphs, src), encoding="utf-8")
    print(f"{dst}: {len(glyphs)} glyphs, {dst.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
