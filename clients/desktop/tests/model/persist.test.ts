import { describe, expect, test } from "bun:test";
import { leaf, splitAt, type DeskTab } from "../../src/workspace/layout";
import {
  defaultDisplay,
  parseLayout,
  sanitizeDisplay,
  saveLayout,
  SIDEBAR_MAX,
} from "../../src/workspace/persist";

describe("layout snapshot", () => {
  test("round-trips split trees, titles and the active tab by terminal identity", () => {
    const root = splitAt(
      leaf({ id: "p1", terminalId: "local:1", viewId: "11" }),
      "p1",
      { id: "p2", terminalId: "local:2", viewId: "12" },
      "column",
    );
    const tabs: DeskTab[] = [{ id: "tab-1", title: "build", root, focusedId: "p2" }];
    const saved = saveLayout("server-a", tabs, defaultDisplay, "tab-1");
    const parsed = parseLayout(JSON.parse(JSON.stringify(saved)));
    expect(parsed).toEqual(saved);
    expect(parsed?.tabs[0]?.focusedTerminal).toBe("local:2");
    expect(parsed?.tabs[0]?.root).toEqual({
      kind: "split",
      axis: "column",
      ratio: 0.5,
      first: { kind: "leaf", terminalId: "local:1" },
      second: { kind: "leaf", terminalId: "local:2" },
    });
  });

  test("migrates version 1 flat tabs into even row splits", () => {
    const parsed = parseLayout({
      version: 1,
      serverId: "s",
      tabs: [{ id: "t", title: "x", terminals: ["a", "b", "c"], focusedTerminal: "b" }],
      display: { fontSize: 15, optionAsAlt: true },
    });
    expect(parsed?.version).toBe(2);
    expect(parsed?.display.fontSize).toBe(15);
    expect(parsed?.display.optionAsAlt).toBe(true);
    const root = parsed?.tabs[0]?.root;
    expect(root?.kind === "split" && root.ratio).toBeCloseTo(1 / 3);
    expect(root?.kind === "split" && root.second.kind === "split" && root.second.ratio).toBe(0.5);
  });

  test("damaged values degrade instead of throwing", () => {
    expect(parseLayout(undefined)).toBeUndefined();
    expect(parseLayout({ version: 9, serverId: "s", tabs: [] })).toBeUndefined();
    const parsed = parseLayout({
      version: 2,
      serverId: "s",
      tabs: [
        {
          id: "ok",
          root: { kind: "split", axis: "row", first: { kind: "leaf", terminalId: "a" }, second: 7 },
        },
        { id: "bad", root: { kind: "split", axis: "diagonal" } },
        "junk",
      ],
      display: "nope",
    });
    expect(parsed?.tabs.map((tab) => tab.id)).toEqual(["ok"]);
    expect(parsed?.tabs[0]?.root).toEqual({ kind: "leaf", terminalId: "a" });
    expect(parsed?.display).toEqual(defaultDisplay);
  });

  test("display prefs are bounded and unknown themes fall back", () => {
    const clean = sanitizeDisplay({
      ...defaultDisplay,
      fontSize: 400,
      lineHeight: Number.NaN,
      sidebarWidth: 9000,
      themeId: "neon",
      fontFamily: "   ",
    });
    expect(clean.fontSize).toBe(28);
    expect(clean.lineHeight).toBe(defaultDisplay.lineHeight);
    expect(clean.sidebarWidth).toBe(SIDEBAR_MAX);
    expect(clean.themeId).toBe(defaultDisplay.themeId);
    expect(clean.fontFamily).toBe(defaultDisplay.fontFamily);
  });
});
