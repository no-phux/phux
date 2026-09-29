import { describe, expect, test } from "bun:test";
import {
  ghosttyAction,
  ghosttyChord,
  ghosttyPrefs,
  ghosttyTheme,
  parseGhostty,
} from "../../src/settings/ghostty";
import { keyChord } from "../../src/shell/keymap";
import { leaf, nudge, splitAt, type LayoutNode } from "../../src/workspace/layout";
import { defaultDisplay } from "../../src/workspace/persist";

const CONFIG = `
# comments and blank lines are ignored

font-family = "JetBrainsMono Nerd Font"
font-family = Symbols Nerd Font
font-size = 14
adjust-cell-width = -8%
adjust-cell-height = 2
background = #071012
foreground = c5d0cd
cursor-color = #45d0bd
selection-background = #1d3035
palette = 0=#081114
palette = 1=#e45f57
palette = 17=#ffffff
window-padding-x = 4
window-padding-y = 4,6
unfocused-split-opacity = 0.85
split-divider-color = #1d3035
macos-option-as-alt = left
keybind = global:ctrl+grave_accent=toggle_quick_terminal
keybind = super+d=new_split:right
keybind = super+ctrl+arrow_left=goto_split:left
keybind = super+alt+arrow_up=resize_split:up,20
keybind = super+plus=increase_font_size:1
keybind = super+3=goto_tab:3
keybind = ctrl+shift+tab=previous_tab
keybind = super+shift+arrow_up=jump_to_prompt:-1
keybind = super+t=unbind
nonsense line without equals
`;

describe("ghostty config", () => {
  const config = parseGhostty(CONFIG);

  test("reads the primary font, sizes and percent-only cell adjustments", () => {
    expect(config.fontFamily).toBe("JetBrainsMono Nerd Font");
    expect(config.fontSize).toBe(14);
    expect(config.cellWidth).toBeCloseTo(0.92);
    expect(config.cellHeight).toBeUndefined();
  });

  test("reads colours with or without #, and ignores out-of-range palette slots", () => {
    expect(config.background).toBe("#071012");
    expect(config.foreground).toBe("#c5d0cd");
    expect(config.palette[0]).toBe("#081114");
    expect(config.palette[1]).toBe("#e45f57");
    expect(config.palette).toHaveLength(16);
    expect(config.paddingY).toBe(4);
    expect(config.unfocusedSplitOpacity).toBe(0.85);
    expect(config.optionAsAlt).toBe(true);
  });

  test("maps keybinds onto commands and reports what has no equivalent", () => {
    expect(config.keybinds.get("cmd+d")).toBe("split-right");
    expect(config.keybinds.get("cmd+ctrl+left")).toBe("pane-left");
    expect(config.keybinds.get("cmd+alt+up")).toBe("resize-up");
    expect(config.keybinds.get("cmd+shift+=")).toBe("font-up");
    expect(config.keybinds.get("cmd+3")).toBe("tab-3");
    expect(config.keybinds.get("ctrl+shift+tab")).toBe("tab-prev");
    expect(config.keybinds.get("ctrl+`")).toBe("quick-terminal");
    expect(config.keybinds.get("cmd+t")).toBe("");
    expect(config.unmapped).toEqual(["super+shift+arrow_up = jump_to_prompt:-1"]);
  });

  test("a theme needs both default colours; a palette needs all 16", () => {
    const theme = ghosttyTheme(config);
    expect(theme?.background).toBe("#071012");
    expect(theme?.accent).toBe("#45d0bd");
    expect(theme?.divider).toBe("#1d3035");
    expect(theme?.palette).toBeUndefined();
    const full = parseGhostty(
      `background = 000000\nforeground = ffffff\n${Array.from({ length: 16 }, (_, index) => `palette = ${index}=#0000${index.toString(16).padStart(2, "0")}`).join("\n")}`,
    );
    expect(ghosttyTheme(full)?.palette).toHaveLength(16);
    expect(ghosttyTheme(parseGhostty("background = 000000"))).toBeUndefined();
  });

  test("prefs adopt the Ghostty font, cell and padding", () => {
    const prefs = ghosttyPrefs(defaultDisplay, config);
    expect(prefs.fontFamily).toBe("JetBrainsMono Nerd Font");
    expect(prefs.lineHeight).toBe(1);
    expect(prefs.cellWidth).toBeCloseTo(0.92);
    expect(prefs.paddingX).toBe(6);
    expect(prefs.unfocusedOpacity).toBe(0.85);
    expect(prefs.themeId).toBe("ghostty");
  });

  test("chords and actions translate or refuse cleanly", () => {
    expect(ghosttyChord("shift+super+bracket_right")).toBe("cmd+shift+]");
    expect(ghosttyChord("ctrl+a>c")).toBeUndefined();
    expect(ghosttyChord("super+")).toBeUndefined();
    expect(ghosttyAction("new_split:down")).toBe("split-down");
    expect(ghosttyAction("resize_split:left,20")).toBe("resize-left");
    expect(ghosttyAction("goto_tab:9")).toBe("tab-9");
    expect(ghosttyAction("write_screen_file:paste")).toBeUndefined();
  });
});

describe("window chords", () => {
  const none = { cmd: false, alt: false, ctrl: false, shift: false };

  test("Control and Option chords reach the keymap; plain typing does not", () => {
    expect(keyChord({ key: "tab", modifiers: { ...none, ctrl: true } })).toBe("ctrl+tab");
    expect(keyChord({ key: "tab", modifiers: { ...none, ctrl: true, shift: true } })).toBe(
      "ctrl+shift+tab",
    );
    expect(keyChord({ key: "a", modifiers: none })).toBe("");
    expect(keyChord({ key: "A", modifiers: { ...none, shift: true } })).toBe("");
    expect(keyChord({ key: "escape", modifiers: none })).toBe("escape");
    expect(keyChord({ key: "k", modifiers: { ...none, cmd: true } })).toBe("cmd+k");
  });
});

describe("divider nudging", () => {
  function place(id: string): { id: string; terminalId: string; viewId: string } {
    return { id, terminalId: id, viewId: id };
  }

  test("moves the nearest split on the matching axis only", () => {
    let root: LayoutNode = splitAt(leaf(place("a")), "a", place("b"), "row");
    root = splitAt(root, "b", place("c"), "column");
    const right = nudge(root, "c", "right", 0.1);
    expect(right.kind === "split" && right.ratio).toBeCloseTo(0.6);
    const up = nudge(root, "c", "up", 0.1);
    expect(up.kind === "split" && up.second.kind === "split" && up.second.ratio).toBeCloseTo(0.4);
    expect(nudge(root, "a", "up", 0.1)).toBe(root);
  });
});
