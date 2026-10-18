import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";
import { emit, group, parseBdf } from "./bitmap.ts";

const ROWS: string[] = Array.from({ length: 16 }, (_, index) => (index === 0 ? "ff" : "00"));

function face(glyphs: string[], bounding = "8 16 0 -4"): string {
  return [
    "STARTFONT 2.1",
    `FONTBOUNDINGBOX ${bounding}`,
    `CHARS ${glyphs.length}`,
    ...glyphs,
    "ENDFONT",
    "",
  ].join("\n");
}

function glyph(encoding: number, bbx = "8 16 0 -4", rows = ROWS): string {
  return [
    `STARTCHAR g${encoding}`,
    `ENCODING ${encoding}`,
    `BBX ${bbx}`,
    "BITMAP",
    ...rows,
    "ENDCHAR",
  ].join("\n");
}

test("the committed table is a fresh regeneration of the vendored face", () => {
  const bdf = readFileSync(repoPath("crates/phux-record/assets/spleen-8x16.bdf"), "utf8");
  const committed = readFileSync(repoPath("crates/phux-record/src/font/spleen_8x16.rs"), "utf8");
  expect(emit(parseBdf(bdf))).toBe(committed);
});

test("small gaps are bridged and wide ones split", () => {
  expect(group([0x20, 0x21, 0x2a])).toEqual([[0x20, 0x2a]]);
  expect(group([0x20, 0x2b])).toEqual([
    [0x20, 0x20],
    [0x2b, 0x2b],
  ]);
});

test("unencoded glyphs are dropped and bridged holes render blank", () => {
  const glyphs = parseBdf(face([glyph(0x41), glyph(-1), glyph(0x43)]));
  expect([...glyphs.keys()]).toEqual([0x41, 0x43]);
  const rust = emit(glyphs);
  expect(rust).toContain("static R_0041_0043: [[u8; 16]; 3] = [");
  expect(rust).toContain(`    [${Array.from({ length: 16 }, () => "0x00").join(",")}], // U+0042`);
  expect(rust).toContain("    (0x0041, 0x0043, &R_0041_0043),");
  expect(rust).toContain("//! columns. 2 glyphs in 1 contiguous ranges.");
});

test("a differently shaped face is refused", () => {
  expect(() => parseBdf(face([glyph(0x41)], "6 12 0 -2"))).toThrow(
    "FONTBOUNDINGBOX is (6, 12), expected (8, 16)",
  );
  expect(() => parseBdf("STARTFONT 2.1\n")).toThrow("FONTBOUNDINGBOX is None");
  expect(() => parseBdf(face([glyph(0x41, "6 16 0 -4")]))).toThrow(
    "glyph U+0041 has BBX 6x16, expected 8x16",
  );
  expect(() => parseBdf(face([glyph(0x41, undefined, ROWS.slice(1))]))).toThrow(
    "glyph U+0041 has 15 bitmap rows, expected 16",
  );
  expect(() => parseBdf(face([glyph(0x41, undefined, ["fff", ...ROWS.slice(1)])]))).toThrow(
    'expected one hex byte, got "fff"',
  );
  expect(() => parseBdf(face([glyph(-1)]))).toThrow("no encoded glyphs found");
});
