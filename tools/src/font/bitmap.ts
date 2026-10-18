// Convert the vendored Spleen BDF face into the committed Rust glyph table
// (ADR-0060: committed output, no build.rs). Each glyph is 16 row bytes, most
// significant bit leftmost, exactly as an 8-wide BDF BITMAP block spells it.
// A face or glyph that is not 8x16 is refused rather than emitted as garbage.
import { Failure } from "../lib/command.ts";

export const SOURCE = "crates/phux-record/assets/spleen-8x16.bdf";
export const TABLE = "crates/phux-record/src/font/spleen_8x16.rs";

const CELL = "8 16";
const ROWS = 16;
// Holes up to this many codepoints are padded with blank glyphs; wider ones
// (Latin to the Private Use Area) split the range instead.
const MAX_GAP = 8;

export type Glyphs = Map<number, number[]>;

const hex4 = (code: number): string => code.toString(16).toUpperCase().padStart(4, "0");

/** Every encoded glyph in the face: codepoint to its 16 row bytes. */
export function parseBdf(text: string): Glyphs {
  const glyphs: Glyphs = new Map();
  let bounding: string | undefined;
  let code = Number.NaN;
  let bbx = "";
  let rows: number[] | undefined;
  for (const [index, raw] of text.split(/\r?\n/).entries()) {
    const at = `line ${index + 1}`;
    const line = raw.trim();
    const [key = "", ...fields] = line.split(/\s+/);
    if (rows === undefined) {
      if (key === "FONTBOUNDINGBOX") bounding = fields.slice(0, 2).join(" ");
      else if (key === "ENCODING") code = Number(fields[0]);
      else if (key === "BBX") bbx = fields.slice(0, 2).join(" ");
      else if (key === "BITMAP") rows = [];
      continue;
    }
    if (line !== "ENDCHAR") {
      if (!/^[0-9a-f]{2}$/i.test(line))
        throw new Failure(`${at}: expected one hex byte, got ${JSON.stringify(line)}`);
      rows.push(Number.parseInt(line, 16));
      continue;
    }
    if (!Number.isInteger(code)) throw new Failure(`${at}: ENDCHAR without an integer ENCODING`);
    if (bbx !== CELL)
      throw new Failure(`${at}: glyph U+${hex4(code)} has BBX ${bbx}, expected ${CELL}`);
    if (rows.length !== ROWS) {
      throw new Failure(
        `${at}: glyph U+${hex4(code)} has ${rows.length} bitmap rows, expected ${ROWS}`,
      );
    }
    // ENCODING -1 marks an unencoded glyph, which no codepoint can reach.
    if (code >= 0) glyphs.set(code, rows);
    [code, bbx, rows] = [Number.NaN, "", undefined];
  }
  if (bounding !== CELL)
    throw new Failure(`FONTBOUNDINGBOX is ${bounding ?? "missing"}, expected ${CELL}`);
  if (glyphs.size === 0) throw new Failure("no encoded glyphs found");
  return glyphs;
}

/** Collapse sorted codepoints into `[start, end]` runs, bridging small gaps. */
export function group(codepoints: readonly number[]): [number, number][] {
  const runs: [number, number][] = [];
  for (const code of codepoints) {
    const last = runs.at(-1);
    if (last !== undefined && code - last[1] - 1 <= MAX_GAP) last[1] = code;
    else runs.push([code, code]);
  }
  return runs;
}

const HEADER = `//! Spleen 8x16 glyph bitmaps, generated -- do not edit by hand.
//!
//! Source: Spleen 2.1.0, <https://www.cambus.net/spleen-monospaced-bitmap-fonts/>,
//! vendored verbatim at \`crates/phux-record/assets/spleen-8x16.bdf\`.
//! Copyright (c) 2018-2024, Frederic Cambus. Licensed BSD-2-Clause, whose
//! redistribution clause requires the copyright notice and disclaimer to travel
//! with both source and binary forms of phux; that notice lives verbatim at
//! \`/THIRD-PARTY-NOTICES.md\` in the repository root. Do not delete that file, and do not
//! swap this face for an OFL-1.1 one -- OFL-1.1 is not on deny.toml's allow list.
//!
//! Regenerate with \`bun tools/main.ts gen-bitmap-font\` after upgrading the .bdf.
//! The generator is documentation of how this file came to be, never a build
//! step: this file is committed and compiled as ordinary Rust.
//!
//! One byte per pixel row, most significant bit at the leftmost of the eight`;

export function emit(glyphs: Glyphs): string {
  const codepoints = [...glyphs.keys()].sort((left, right) => left - right);
  const runs = group(codepoints);
  const blank = Array.from({ length: ROWS }, () => 0);
  const name = ([start, end]: [number, number]): string => `R_${hex4(start)}_${hex4(end)}`;
  const out = [
    HEADER,
    `//! columns. ${codepoints.length} glyphs in ${runs.length} contiguous ranges.`,
    "",
  ];
  for (const run of runs) {
    const [start, end] = run;
    out.push("#[rustfmt::skip]", `static ${name(run)}: [[u8; ${ROWS}]; ${end - start + 1}] = [`);
    for (let code = start; code <= end; code++) {
      const body = (glyphs.get(code) ?? blank).map(
        (byte) => `0x${byte.toString(16).padStart(2, "0")}`,
      );
      out.push(`    [${body.join(",")}], // U+${hex4(code)}`);
    }
    out.push("];", "");
  }
  out.push(
    "/// Sorted, non-overlapping `(start, end, bitmaps)` groups.",
    "#[rustfmt::skip]",
    `pub(super) static RANGES: [(u32, u32, &[[u8; ${ROWS}]]); ${runs.length}] = [`,
    ...runs.map((run) => `    (0x${hex4(run[0])}, 0x${hex4(run[1])}, &${name(run)}),`),
    "];",
    "",
  );
  return out.join("\n");
}
