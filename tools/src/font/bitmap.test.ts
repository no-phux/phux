import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";
import { SOURCE, TABLE, emit, group, parseBdf } from "./bitmap.ts";
import { firstDifference } from "./commands.ts";

const ROWS: string[] = Array.from({ length: 16 }, (_, index) => (index === 0 ? "ff" : "00"));

function face(glyphs: string[], bounding = "8 16 0 -4"): string {
  return ["STARTFONT 2.1", `FONTBOUNDINGBOX ${bounding}`, ...glyphs, "ENDFONT", ""].join("\n");
}

function glyph(encoding: number, bbx = "8 16 0 -4", rows = ROWS): string {
  return [`STARTCHAR g`, `ENCODING ${encoding}`, `BBX ${bbx}`, "BITMAP", ...rows, "ENDCHAR"].join(
    "\r\n",
  );
}

test("the committed table is a fresh regeneration, and drift is located", () => {
  const committed = readFileSync(repoPath(TABLE), "utf8");
  const fresh = emit(parseBdf(readFileSync(repoPath(SOURCE), "utf8")));
  expect(firstDifference(committed, fresh)).toBeUndefined();
  const edited = committed.replace("0x00", "0x01");
  expect(firstDifference(edited, fresh)).toMatch(/^line \d+:\n- .*0x01.*\n\+ /);
  expect(firstDifference(committed.split("\n").slice(0, 3).join("\n"), fresh)).toContain(
    "line 4:\n- (end of file)",
  );
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
});

test.each([
  [face([glyph(0x41)], "6 12 0 -2"), "FONTBOUNDINGBOX is 6 12, expected 8 16"],
  ["STARTFONT 2.1\n", "FONTBOUNDINGBOX is missing"],
  [face([glyph(0x41, "6 16 0 -4")]), "glyph U+0041 has BBX 6 16, expected 8 16"],
  [face([glyph(0x41, undefined, ROWS.slice(1))]), "glyph U+0041 has 15 bitmap rows, expected 16"],
  [face([glyph(0x41, undefined, ["fff", ...ROWS.slice(1)])]), 'expected one hex byte, got "fff"'],
  [face([glyph(-1)]), "no encoded glyphs found"],
])("a malformed face is refused: %#", (text, message) => {
  expect(() => parseBdf(text)).toThrow(message);
});
