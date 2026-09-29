import { describe, expect, test } from "bun:test";
import {
  clampRatio,
  cycle,
  equalize,
  layoutRects,
  leaf,
  neighbor,
  placements,
  refocus,
  removeLeaf,
  reorder,
  setRatio,
  splitAt,
  withoutTerminal,
  type DeskTab,
  type LayoutNode,
  type Placement,
} from "../../src/workspace/layout";

function place(id: string, terminalId = `t-${id}`): Placement {
  return { id, terminalId, viewId: `v-${id}` };
}

function ids(node: LayoutNode | undefined): string[] {
  return node ? placements(node).map((placement) => placement.id) : [];
}

/** a | (b / c): a on the left, b over c on the right. */
function grid(): LayoutNode {
  let root = leaf(place("a"));
  root = splitAt(root, "a", place("b"), "row");
  return splitAt(root, "b", place("c"), "column");
}

describe("split tree", () => {
  test("splitting a leaf keeps reading order and existing leaf objects", () => {
    const a = leaf(place("a"));
    const root = splitAt(a, "a", place("b"), "row");
    expect(ids(root)).toEqual(["a", "b"]);
    expect(root.kind === "split" && root.first).toBe(a);
  });

  test("a split before the target puts the new leaf left of or above it", () => {
    const root = splitAt(grid(), "c", place("d"), "column", true);
    expect(ids(root)).toEqual(["a", "b", "d", "c"]);
    const rects = layoutRects(root, { x: 0, y: 0, width: 1, height: 1 });
    expect(rects.get("d")?.y).toBeLessThan(rects.get("c")?.y ?? 0);
  });

  test("removing a leaf promotes its sibling into the parent's rect", () => {
    const root = grid();
    const without = removeLeaf(root, "b");
    expect(ids(without)).toEqual(["a", "c"]);
    expect(without?.kind === "split" && without.second.kind).toBe("leaf");
    expect(removeLeaf(leaf(place("x")), "x")).toBeUndefined();
  });

  test("ratios clamp and equalize resets every split", () => {
    const root = grid();
    const splitId = root.kind === "split" ? root.id : "";
    const skewed = setRatio(root, splitId, 5);
    expect(skewed.kind === "split" && skewed.ratio).toBe(clampRatio(5));
    const even = equalize(skewed);
    expect(even.kind === "split" && even.ratio).toBe(0.5);
    expect(clampRatio(Number.NaN)).toBe(0.5);
  });

  test("rects tile the bounds exactly", () => {
    const rects = layoutRects(grid(), { x: 0, y: 0, width: 100, height: 60 });
    expect(rects.get("a")).toEqual({ x: 0, y: 0, width: 50, height: 60 });
    expect(rects.get("b")).toEqual({ x: 50, y: 0, width: 50, height: 30 });
    expect(rects.get("c")).toEqual({ x: 50, y: 30, width: 50, height: 30 });
  });

  test("directional focus follows geometry, not tree order", () => {
    const root = grid();
    expect(neighbor(root, "a", "right")).toBe("b");
    expect(neighbor(root, "c", "left")).toBe("a");
    expect(neighbor(root, "b", "down")).toBe("c");
    expect(neighbor(root, "c", "up")).toBe("b");
    expect(neighbor(root, "a", "left")).toBeUndefined();
    expect(neighbor(root, "a", "up")).toBeUndefined();
  });

  test("cycling wraps in reading order", () => {
    const root = grid();
    expect(cycle(root, "c", 1)).toBe("a");
    expect(cycle(root, "a", -1)).toBe("c");
  });
});

describe("tabs", () => {
  function tab(root: LayoutNode, focusedId: string, zoomedId?: string): DeskTab {
    const value: DeskTab = { id: "tab", root, focusedId };
    if (zoomedId) value.zoomedId = zoomedId;
    return value;
  }

  test("a closed terminal leaves every tab that showed it, and empty tabs disappear", () => {
    const shared = splitAt(leaf(place("a", "t1")), "a", place("b", "t1"), "row");
    const other = splitAt(leaf(place("c", "t2")), "c", place("d", "t1"), "column");
    const next = withoutTerminal([tab(shared, "a"), { ...tab(other, "d"), id: "other" }], "t1");
    expect(next.map((item) => item.id)).toEqual(["other"]);
    expect(ids(next[0]?.root)).toEqual(["c"]);
    expect(next[0]?.focusedId).toBe("c");
  });

  test("refocus drops a zoom that no longer points at a leaf", () => {
    const fixed = refocus(tab(leaf(place("a")), "gone", "gone"));
    expect(fixed.focusedId).toBe("a");
    expect(fixed.zoomedId).toBeUndefined();
  });

  test("reorder clamps and preserves members", () => {
    expect(reorder(["a", "b", "c"], 0, 2)).toEqual(["b", "c", "a"]);
    expect(reorder(["a", "b", "c"], 2, -4)).toEqual(["c", "a", "b"]);
    expect(reorder(["a"], 3, 0)).toEqual(["a"]);
  });
});
