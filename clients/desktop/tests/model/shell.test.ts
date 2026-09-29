import { describe, expect, test } from "bun:test";
import { rank } from "../../src/shell/filter";
import { chordOf, displayChord } from "../../src/shell/keymap";
import { quotePaths } from "../../src/terminal/pane";
import { mix, palette, readableOn, themes } from "../../src/ui/theme";

const mods = { cmd: true, alt: false, ctrl: false, shift: false };

describe("chords", () => {
  test("only Command chords belong to the app, and repeats never fire", () => {
    expect(chordOf({ key: "t", modifiers: mods })).toBe("cmd+t");
    expect(chordOf({ key: "t", modifiers: { ...mods, cmd: false } })).toBe("");
    expect(chordOf({ key: "t", isHeld: true, modifiers: mods })).toBe("");
  });

  test("shifted punctuation and capitals normalize to the unshifted key", () => {
    expect(chordOf({ key: "}", modifiers: { ...mods, shift: true } })).toBe("cmd+shift+]");
    expect(chordOf({ key: "+", modifiers: mods })).toBe("cmd+shift+=");
    expect(chordOf({ key: "G", modifiers: mods })).toBe("cmd+shift+g");
    expect(chordOf({ key: "left", modifiers: { ...mods, alt: true } })).toBe("cmd+alt+left");
  });

  test("display uses macOS modifier order", () => {
    expect(displayChord("cmd+shift+p")).toBe("⇧⌘P");
    expect(displayChord("cmd+alt+left")).toBe("⌥⌘←");
  });
});

describe("palette ranking", () => {
  const items = ["Split Right", "Split Down", "Settings", "Scroll to Live Output", "Theme: Nord"];

  test("every token must match; word starts outrank inner hits", () => {
    expect(rank(items, "split d", (item) => item)).toEqual(["Split Down"]);
    expect(rank(items, "s", (item) => item)[0]).toBe("Split Right");
    expect(rank(items, "nord", (item) => item)).toEqual(["Theme: Nord"]);
  });

  test("subsequences match after substrings, and empty queries keep order", () => {
    expect(rank(items, "sttg", (item) => item)).toEqual(["Settings"]);
    expect(rank(items, "", (item) => item)).toEqual(items);
    expect(rank(items, "zzz", (item) => item)).toEqual([]);
  });
});

describe("dropped paths", () => {
  test("paste as quoted text, never as shell syntax", () => {
    expect(quotePaths(["/tmp/a.txt", "/Users/me/My File's.png"])).toBe(
      "/tmp/a.txt '/Users/me/My File'\\''s.png'",
    );
    expect(quotePaths(["/x/$(rm -rf ~)"])).toBe("'/x/$(rm -rf ~)'");
  });
});

describe("theme tokens", () => {
  test("mixing is linear per channel", () => {
    expect(mix("#000000", "#ffffff", 0.5)).toBe("#808080");
    expect(mix("#102030", "#102030", 0.7)).toBe("#102030");
  });

  test("text on an accent picks the higher-contrast ink", () => {
    expect(readableOn("#ffffff", "#000000", "#eeeeee")).toBe("#000000");
    expect(readableOn("#101010", "#000000", "#ffffff")).toBe("#ffffff");
  });

  test("every built-in theme expands to valid hex tokens", () => {
    for (const theme of themes) {
      const tokens = palette(theme);
      for (const key of [
        "raised",
        "border",
        "hover",
        "active",
        "subtext",
        "muted",
        "faint",
        "accentWash",
      ] as const) {
        expect(tokens[key]).toMatch(/^#[0-9a-f]{6}$/);
      }
    }
  });
});
