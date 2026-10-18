// Convert the vendored Spleen BDF face into a Rust const glyph table.
//
// This is *reproducibility documentation*, not a build step. There is no
// build.rs anywhere in this workspace, and there is deliberately none here:
// the generated file is committed, so a normal `cargo build` compiles a plain
// Rust source file with no BDF parser and no codegen in the dependency graph.
// Run this only when the vendored face is upgraded, then commit the
// regenerated .rs alongside the new .bdf:
//
//     bun tools/main.ts gen-bitmap-font \
//         crates/phux-record/assets/spleen-8x16.bdf \
//         crates/phux-record/src/font/spleen_8x16.rs
//
// Output shape, matching `phux_record::font::BitmapFont::ranges`: a sorted,
// non-overlapping table of `(start_codepoint, end_codepoint, &[[u8; 16]])`
// groups. Each glyph is 16 bytes, one per pixel row, most significant bit at
// the leftmost of the eight columns -- the same orientation the BDF BITMAP
// block already uses for an 8-pixel-wide face, so the conversion is a
// straight hex-parse with no bit reversal.
//
// The BDF is parsed rather than trusted blindly: a face whose FONTBOUNDINGBOX
// is not 8x16, or a glyph whose BBX disagrees with it, is a hard error.
// Silently accepting a differently-shaped face would produce a table that
// compiles and renders garbage, which is far worse than refusing to generate.
import { Failure } from "../lib/command.ts";

export const CELL_W = 8;
export const CELL_H = 16;

// Ranges wider than this many missing codepoints are split rather than padded
// with blanks. Padding a handful of holes keeps the range table short (fewer
// binary-search steps, less source noise); padding a 60k-codepoint hole
// between Latin and the Private Use Area would embed megabytes of zeroes.
const MAX_GAP = 8;

export class FontError extends Failure {}

export type Glyphs = Map<number, number[]>;

// The line breaks Python's str.splitlines() honours, which the generator
// has always used to read the face; the controls among them are deliberate.
// oxlint-disable-next-line no-control-regex
const LINE_BREAK = /\r\n|[\n\v\f\r\x1c\x1d\x1e\x85\u2028\u2029]/;

function int(text: string | undefined, line: number): number {
  if (text === undefined || !/^[+-]?\d+$/.test(text)) {
    throw new FontError(`${line}: expected an integer, got ${JSON.stringify(text ?? "")}`);
  }
  return Number(text);
}

function hex4(code: number): string {
  return code.toString(16).toUpperCase().padStart(4, "0");
}

interface Pending {
  encoding: number | undefined;
  bbx: [number, number] | undefined;
  rows: number[] | undefined;
}

/** Validate a finished glyph; returns it unless it is unencoded. */
function finish(pending: Pending, line: number): [number, number[]] | undefined {
  const { encoding, bbx, rows } = pending;
  if (encoding === undefined || bbx === undefined || rows === undefined) {
    throw new FontError(`${line}: ENDCHAR with no ENCODING/BBX`);
  }
  if (bbx[0] !== CELL_W || bbx[1] !== CELL_H) {
    throw new FontError(
      `${line}: glyph U+${hex4(encoding)} has BBX ${bbx[0]}x${bbx[1]}, expected ${CELL_W}x${CELL_H}`,
    );
  }
  if (rows.length !== CELL_H) {
    throw new FontError(
      `${line}: glyph U+${hex4(encoding)} has ${rows.length} bitmap rows, expected ${CELL_H}`,
    );
  }
  // ENCODING -1 marks an unencoded glyph; it has no codepoint to look up by,
  // so it cannot be reached and is dropped.
  return encoding >= 0 ? [encoding, rows] : undefined;
}

/** Every encoded glyph in the face: codepoint to its 16 row bytes. */
export function parseBdf(text: string): Glyphs {
  const glyphs: Glyphs = new Map();
  let pending: Pending = { encoding: undefined, bbx: undefined, rows: undefined };
  let bounding: [number, number] | undefined;
  const lines = text.split(LINE_BREAK);
  // splitlines() yields no empty last line for a trailing newline.
  if (lines.at(-1) === "") lines.pop();

  for (const [index, raw] of lines.entries()) {
    const lineno = index + 1;
    const line = raw.trim();
    if (pending.rows !== undefined) {
      if (line === "ENDCHAR") {
        const glyph = finish(pending, lineno);
        if (glyph !== undefined) glyphs.set(...glyph);
        pending = { encoding: undefined, bbx: undefined, rows: undefined };
        continue;
      }
      // An 8-wide glyph is exactly one hex byte per row. Wider faces pad to a
      // byte boundary; we reject those in the BBX check, so anything else
      // here is a malformed file.
      if (!/^[0-9a-fA-F]{2}$/.test(line)) {
        throw new FontError(`${lineno}: expected one hex byte, got ${JSON.stringify(line)}`);
      }
      pending.rows.push(Number.parseInt(line, 16));
      continue;
    }

    const parts = line.split(/\s+/);
    if (line.startsWith("FONTBOUNDINGBOX ")) {
      bounding = [int(parts[1], lineno), int(parts[2], lineno)];
    } else if (line.startsWith("ENCODING ")) {
      pending.encoding = int(parts[1], lineno);
    } else if (line.startsWith("BBX ")) {
      // Width, height, x and y offsets; only the cell size is checked.
      const box = [1, 2, 3, 4].map((index) => int(parts[index], lineno));
      pending.bbx = [box[0] ?? 0, box[1] ?? 0];
    } else if (line === "BITMAP") {
      pending.rows = [];
    }
  }

  if (bounding?.[0] !== CELL_W || bounding[1] !== CELL_H) {
    const found = bounding === undefined ? "None" : `(${bounding[0]}, ${bounding[1]})`;
    throw new FontError(`FONTBOUNDINGBOX is ${found}, expected (${CELL_W}, ${CELL_H})`);
  }
  if (glyphs.size === 0) throw new FontError("no encoded glyphs found");
  return glyphs;
}

/** Collapse sorted codepoints into `[start, end]` runs, bridging small gaps. */
export function group(codepoints: readonly number[]): [number, number][] {
  const runs: [number, number][] = [];
  const [first, ...rest] = codepoints;
  if (first === undefined) return runs;
  let start = first;
  let prev = first;
  for (const code of rest) {
    if (code - prev - 1 <= MAX_GAP) {
      prev = code;
      continue;
    }
    runs.push([start, prev]);
    start = prev = code;
  }
  runs.push([start, prev]);
  return runs;
}

const HEADER = [
  "//! Spleen 8x16 glyph bitmaps, generated -- do not edit by hand.",
  "//!",
  "//! Source: Spleen 2.1.0, <https://www.cambus.net/spleen-monospaced-bitmap-fonts/>,",
  "//! vendored verbatim at `crates/phux-record/assets/spleen-8x16.bdf`.",
  "//! Copyright (c) 2018-2024, Frederic Cambus. Licensed BSD-2-Clause, whose",
  "//! redistribution clause requires the copyright notice and disclaimer to travel",
  "//! with both source and binary forms of phux; that notice lives verbatim at",
  "//! `/THIRD-PARTY-NOTICES.md` in the repository root. Do not delete that file, and do not",
  "//! swap this face for an OFL-1.1 one -- OFL-1.1 is not on deny.toml's allow list.",
  "//!",
  "//! Regenerate with `bun tools/main.ts gen-bitmap-font` after upgrading the .bdf.",
  "//! The generator is documentation of how this file came to be, never a build",
  "//! step: this file is committed and compiled as ordinary Rust.",
  "//!",
  "//! One byte per pixel row, most significant bit at the leftmost of the eight",
];

export function emit(glyphs: Glyphs): string {
  const codepoints = [...glyphs.keys()].sort((left, right) => left - right);
  const runs = group(codepoints);
  const blank: number[] = Array.from({ length: CELL_H }, () => 0);
  const out = [
    ...HEADER,
    `//! columns. ${codepoints.length} glyphs in ${runs.length} contiguous ranges.`,
    "",
  ];

  const names: string[] = [];
  for (const [start, end] of runs) {
    const name = `R_${hex4(start)}_${hex4(end)}`;
    names.push(name);
    out.push("#[rustfmt::skip]", `static ${name}: [[u8; ${CELL_H}]; ${end - start + 1}] = [`);
    for (let code = start; code <= end; code++) {
      // Bridged gaps get an all-zero glyph: it renders as a blank cell, which
      // is the right answer for a codepoint the face never had.
      const body = (glyphs.get(code) ?? blank)
        .map((byte) => `0x${byte.toString(16).padStart(2, "0")}`)
        .join(",");
      out.push(`    [${body}], // U+${hex4(code)}`);
    }
    out.push("];", "");
  }

  out.push(
    "/// Sorted, non-overlapping `(start, end, bitmaps)` groups.",
    "#[rustfmt::skip]",
    `pub(super) static RANGES: [(u32, u32, &[[u8; ${CELL_H}]]); ${runs.length}] = [`,
  );
  for (const [index, [start, end]] of runs.entries()) {
    out.push(`    (0x${hex4(start)}, 0x${hex4(end)}, &${names[index] ?? ""}),`);
  }
  out.push("];", "");
  return out.join("\n");
}
