/**
 * Tabs own a binary split tree of placements. A placement is local
 * presentation: one runtime view of one daemon terminal. Every operation here
 * is pure so the layout rules are testable without a window or a server.
 */
export interface Placement {
  id: string;
  terminalId: string;
  viewId: string;
}

export type Axis = "row" | "column";
export type Direction = "left" | "right" | "up" | "down";

export type LayoutNode =
  | { kind: "leaf"; placement: Placement }
  | { kind: "split"; id: string; axis: Axis; ratio: number; first: LayoutNode; second: LayoutNode };

export interface DeskTab {
  id: string;
  /** User-assigned name; absent means "follow the focused terminal's title". */
  title?: string;
  root: LayoutNode;
  focusedId: string;
  zoomedId?: string;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export const MIN_RATIO = 0.08;

let counter = 0;

export function newId(prefix: string): string {
  counter += 1;
  return `${prefix}-${Date.now().toString(36)}-${counter}`;
}

export function leaf(placement: Placement): LayoutNode {
  return { kind: "leaf", placement };
}

export function placements(node: LayoutNode): Placement[] {
  if (node.kind === "leaf") return [node.placement];
  return [...placements(node.first), ...placements(node.second)];
}

export function findPlacement(node: LayoutNode, id: string): Placement | undefined {
  return placements(node).find((placement) => placement.id === id);
}

export function focusedPlacement(tab: DeskTab): Placement | undefined {
  return findPlacement(tab.root, tab.focusedId) ?? placements(tab.root)[0];
}

/** Put `placement` beside `targetId`, splitting that leaf along `axis`. */
export function splitAt(
  node: LayoutNode,
  targetId: string,
  placement: Placement,
  axis: Axis,
): LayoutNode {
  if (node.kind === "leaf") {
    if (node.placement.id !== targetId) return node;
    return {
      kind: "split",
      id: newId("split"),
      axis,
      ratio: 0.5,
      first: node,
      second: leaf(placement),
    };
  }
  return {
    ...node,
    first: splitAt(node.first, targetId, placement, axis),
    second: splitAt(node.second, targetId, placement, axis),
  };
}

/** Remove a leaf; its sibling takes the parent's whole rect. Undefined when empty. */
export function removeLeaf(node: LayoutNode, id: string): LayoutNode | undefined {
  if (node.kind === "leaf") return node.placement.id === id ? undefined : node;
  const first = removeLeaf(node.first, id);
  const second = removeLeaf(node.second, id);
  if (!first) return second;
  if (!second) return first;
  return { ...node, first, second };
}

export function setRatio(node: LayoutNode, splitId: string, ratio: number): LayoutNode {
  if (node.kind === "leaf") return node;
  if (node.id === splitId) return { ...node, ratio: clampRatio(ratio) };
  return {
    ...node,
    first: setRatio(node.first, splitId, ratio),
    second: setRatio(node.second, splitId, ratio),
  };
}

/** Reset every split in the tree to an even division. */
export function equalize(node: LayoutNode): LayoutNode {
  if (node.kind === "leaf") return node;
  return { ...node, ratio: 0.5, first: equalize(node.first), second: equalize(node.second) };
}

export function clampRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return 0.5;
  return Math.min(1 - MIN_RATIO, Math.max(MIN_RATIO, ratio));
}

/** Leaf rectangles inside `bounds`, in the same proportions the view renders. */
export function layoutRects(node: LayoutNode, bounds: Rect): Map<string, Rect> {
  const rects = new Map<string, Rect>();
  const visit = (current: LayoutNode, rect: Rect): void => {
    if (current.kind === "leaf") {
      rects.set(current.placement.id, rect);
      return;
    }
    const [first, second] = divide(rect, current.axis, current.ratio);
    visit(current.first, first);
    visit(current.second, second);
  };
  visit(node, bounds);
  return rects;
}

function divide(rect: Rect, axis: Axis, ratio: number): [Rect, Rect] {
  if (axis === "row") {
    const width = rect.width * ratio;
    return [
      { ...rect, width },
      { ...rect, x: rect.x + width, width: rect.width - width },
    ];
  }
  const height = rect.height * ratio;
  return [
    { ...rect, height },
    { ...rect, y: rect.y + height, height: rect.height - height },
  ];
}

/**
 * The nearest leaf in `direction` from `fromId`: it must lie past the source
 * edge and overlap it on the perpendicular axis; ties go to the most overlap.
 */
export function neighbor(
  node: LayoutNode,
  fromId: string,
  direction: Direction,
): string | undefined {
  const rects = layoutRects(node, { x: 0, y: 0, width: 1, height: 1 });
  const from = rects.get(fromId);
  if (!from) return undefined;
  let best: { id: string; distance: number; overlap: number } | undefined;
  for (const [id, rect] of rects) {
    if (id === fromId) continue;
    const distance = gap(from, rect, direction);
    const overlap = crossOverlap(from, rect, direction);
    if (distance < -1e-9 || overlap <= 1e-9) continue;
    if (
      !best ||
      distance < best.distance - 1e-9 ||
      (Math.abs(distance - best.distance) < 1e-9 && overlap > best.overlap)
    ) {
      best = { id, distance, overlap };
    }
  }
  return best?.id;
}

function gap(from: Rect, to: Rect, direction: Direction): number {
  switch (direction) {
    case "left":
      return from.x - (to.x + to.width);
    case "right":
      return to.x - (from.x + from.width);
    case "up":
      return from.y - (to.y + to.height);
    case "down":
      return to.y - (from.y + from.height);
  }
}

function crossOverlap(from: Rect, to: Rect, direction: Direction): number {
  if (direction === "left" || direction === "right") {
    return Math.min(from.y + from.height, to.y + to.height) - Math.max(from.y, to.y);
  }
  return Math.min(from.x + from.width, to.x + to.width) - Math.max(from.x, to.x);
}

/** Next or previous leaf in reading order, wrapping. */
export function cycle(node: LayoutNode, fromId: string, step: 1 | -1): string | undefined {
  const order = placements(node);
  const index = order.findIndex((placement) => placement.id === fromId);
  if (order.length === 0) return undefined;
  const next = order[(Math.max(index, 0) + step + order.length) % order.length];
  return next?.id;
}

/** Drop every leaf that shows `terminalId`. Tabs left empty disappear. */
export function withoutTerminal(tabs: DeskTab[], terminalId: string): DeskTab[] {
  return tabs.flatMap((tab) => {
    let root: LayoutNode | undefined = tab.root;
    for (const placement of placements(tab.root)) {
      if (placement.terminalId === terminalId && root) root = removeLeaf(root, placement.id);
    }
    if (!root) return [];
    return [refocus({ ...tab, root })];
  });
}

/** Keep the focus and zoom pointers on leaves that still exist. */
export function refocus(tab: DeskTab): DeskTab {
  const ids = new Set(placements(tab.root).map((placement) => placement.id));
  const first = placements(tab.root)[0];
  const focusedId = ids.has(tab.focusedId) ? tab.focusedId : (first?.id ?? "");
  const next: DeskTab = { ...tab, focusedId };
  if (!tab.zoomedId || !ids.has(tab.zoomedId)) delete next.zoomedId;
  return next;
}

/** Move `tabs[from]` to position `to`, clamped. */
export function reorder<T>(items: readonly T[], from: number, to: number): T[] {
  const next = [...items];
  const [moved] = next.splice(from, 1);
  if (moved === undefined) return next;
  next.splice(Math.max(0, Math.min(to, next.length)), 0, moved);
  return next;
}

/**
 * Move the divider nearest `leafId` along `direction`'s axis by `delta`
 * (a ratio), the way Ghostty's `resize_split` does: `right`/`down` push the
 * divider right/down whichever side the leaf is on. Unchanged when no
 * enclosing split runs along that axis.
 */
export function nudge(
  node: LayoutNode,
  leafId: string,
  direction: Direction,
  delta: number,
): LayoutNode {
  const axis: Axis = direction === "left" || direction === "right" ? "row" : "column";
  const sign = direction === "right" || direction === "down" ? 1 : -1;
  const target = pathTo(node, leafId)
    ?.reverse()
    .find((split) => split.axis === axis);
  return target ? setRatio(node, target.id, target.ratio + sign * delta) : node;
}

type SplitNode = Extract<LayoutNode, { kind: "split" }>;

function pathTo(node: LayoutNode, leafId: string): SplitNode[] | undefined {
  if (node.kind === "leaf") return node.placement.id === leafId ? [] : undefined;
  const below = pathTo(node.first, leafId) ?? pathTo(node.second, leafId);
  return below ? [node, ...below] : undefined;
}
