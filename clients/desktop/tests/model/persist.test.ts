import { describe, expect, test } from "bun:test";
import { leaf, splitAt, type DeskTab } from "../../src/workspace/layout";
import {
  defaultDisplay,
  loadLayoutStore,
  parseLayout,
  sanitizeDisplay,
  saveLayout,
  SIDEBAR_MAX,
} from "../../src/workspace/persist";

describe("layout snapshot", () => {
  test("defaults to Paper Mono without replacing an explicit saved face", () => {
    expect(defaultDisplay.fontFamily).toBe("Paper Mono");
    expect(sanitizeDisplay({ ...defaultDisplay, fontFamily: "" }).fontFamily).toBe("Paper Mono");
    expect(sanitizeDisplay({ ...defaultDisplay, fontFamily: "Menlo" }).fontFamily).toBe("Menlo");
  });

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
      first: { kind: "leaf", id: "p1", terminalId: "local:1" },
      second: { kind: "leaf", id: "p2", terminalId: "local:2" },
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

  test("refuses damaged trees instead of silently discarding their leaves", () => {
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
    expect(parsed).toBeUndefined();
  });

  test("unreadable stored layouts survive both workspace and preference writes", () => {
    const unreadable: unknown[] = [
      { version: 99, serverId: "s", tabs: [] },
      {
        version: 2,
        serverId: "s",
        tabs: [{ id: "old-shell", placements: [{ id: "p", terminalId: "local:1" }] }],
      },
      { version: 2, serverId: "s", tabs: [{ id: "t", root: { kind: "leaf" } }] },
      null,
      "{broken json",
    ];
    for (const raw of unreadable) {
      let stored = raw;
      const store = loadLayoutStore({
        read: () => stored,
        write: (layout) => {
          stored = layout;
        },
      });
      store.write(saveLayout("s", [], defaultDisplay));
      store.write(saveLayout("s", [], { ...defaultDisplay, fontSize: 20 }));
      expect(stored).toBe(raw);
    }
  });

  test("an absent store and valid old v2 snapshots remain writable", () => {
    const legacy = {
      version: 2,
      serverId: "s",
      tabs: [
        { id: "t", root: { kind: "leaf", terminalId: "local:1" }, focusedTerminal: "local:1" },
      ],
    };
    for (const raw of [undefined, legacy]) {
      let stored: unknown = raw;
      const store = loadLayoutStore({
        read: () => stored,
        write: (layout) => {
          stored = layout;
        },
      });
      const next = saveLayout("s", [], defaultDisplay);
      store.write(next);
      expect(stored).toEqual(next);
    }
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
