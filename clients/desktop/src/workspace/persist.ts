/**
 * Versioned local presentation snapshot. It remembers placement and display
 * preferences only: never credentials, live anchors or process state, and it
 * binds to exactly one daemon incarnation (`serverId`). Parsing is total: a
 * damaged or foreign value degrades to defaults instead of throwing.
 */
import { defaultThemeId, themes } from "../ui/theme";
import type { Axis, DeskTab, LayoutNode } from "./layout";
import { placements } from "./layout";

export interface DisplayPrefs {
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
  optionAsAlt: boolean;
  themeId: string;
  sidebarVisible: boolean;
  sidebarWidth: number;
  notifications: boolean;
  /** Cell size multipliers, like Ghostty's adjust-cell-width/height. */
  cellWidth: number;
  cellHeight: number;
  /** Space between a pane's edge and its terminal grid, in points. */
  paddingX: number;
  paddingY: number;
  /** Opacity kept by panes that are not focused (1 = no dimming). */
  unfocusedOpacity: number;
  /** Apply the Ghostty config's keybinds over the built-in ones. */
  ghosttyKeys: boolean;
}

export const defaultDisplay: DisplayPrefs = {
  fontFamily: "Paper Mono",
  fontSize: 13,
  lineHeight: 1.3,
  optionAsAlt: false,
  themeId: defaultThemeId,
  sidebarVisible: true,
  sidebarWidth: 248,
  notifications: true,
  cellWidth: 1,
  cellHeight: 1,
  paddingX: 8,
  paddingY: 6,
  unfocusedOpacity: 1,
  ghosttyKeys: true,
};

export const fontFamilies = [
  "Paper Mono",
  "Menlo",
  "SF Mono",
  "Monaco",
  "JetBrains Mono",
  "Fira Code",
  "Berkeley Mono",
  "Iosevka",
] as const;

export const SIDEBAR_MIN = 180;
export const SIDEBAR_MAX = 440;

export type SavedNode =
  | { kind: "leaf"; terminalId: string; id?: string }
  | { kind: "split"; axis: Axis; ratio: number; first: SavedNode; second: SavedNode };

export interface SavedTab {
  id: string;
  title?: string;
  root: SavedNode;
  focusedTerminal: string;
  /** Optional for older v2 snapshots that only recorded terminal focus. */
  focusedId?: string;
}

export interface SavedLayout {
  version: 2;
  serverId: string;
  activeTab?: string;
  tabs: SavedTab[];
  display: DisplayPrefs;
}

/** Refuse automatic replacement of data this version cannot read losslessly. */
export function loadLayoutStore(store: { read(): unknown; write(layout: unknown): void }): {
  initial: SavedLayout | undefined;
  blocked: boolean;
  write(layout: SavedLayout): void;
} {
  const raw = store.read();
  const initial = parseLayout(raw);
  const blocked = raw !== undefined && initial === undefined;
  return {
    initial,
    blocked,
    write: (layout) => {
      if (!blocked) store.write(layout);
    },
  };
}

export function saveLayout(
  serverId: string,
  tabs: DeskTab[],
  display: DisplayPrefs,
  activeTab?: string,
): SavedLayout {
  const layout: SavedLayout = {
    version: 2,
    serverId,
    display: sanitizeDisplay(display),
    tabs: tabs.map((tab) => {
      const focused = placements(tab.root).find((placement) => placement.id === tab.focusedId);
      const saved: SavedTab = {
        id: tab.id,
        root: saveNode(tab.root),
        focusedTerminal: focused?.terminalId ?? "",
        focusedId: tab.focusedId,
      };
      if (tab.title) saved.title = tab.title;
      return saved;
    }),
  };
  if (activeTab) layout.activeTab = activeTab;
  return layout;
}

function saveNode(node: LayoutNode): SavedNode {
  if (node.kind === "leaf")
    return { kind: "leaf", id: node.placement.id, terminalId: node.placement.terminalId };
  return {
    kind: "split",
    axis: node.axis,
    ratio: node.ratio,
    first: saveNode(node.first),
    second: saveNode(node.second),
  };
}

/** Accepts version 2, and migrates version 1's flat terminal lists into row splits. */
export function parseLayout(value: unknown): SavedLayout | undefined {
  if (!isRecord(value) || typeof value.serverId !== "string" || !Array.isArray(value.tabs)) {
    return undefined;
  }
  const tabs =
    value.version === 2
      ? value.tabs.flatMap(parseTab)
      : value.version === 1
        ? value.tabs.flatMap(migrateTab)
        : undefined;
  if (!tabs || tabs.length !== value.tabs.length || !uniqueIdentities(tabs)) return undefined;
  const layout: SavedLayout = {
    version: 2,
    serverId: value.serverId,
    tabs,
    display: parseDisplay(value.display),
  };
  if (typeof value.activeTab === "string") layout.activeTab = value.activeTab;
  return layout;
}

function savedIds(node: SavedNode): string[] {
  if (node.kind === "leaf") return node.id === undefined ? [] : [node.id];
  return [...savedIds(node.first), ...savedIds(node.second)];
}

function uniqueIdentities(tabs: SavedTab[]): boolean {
  const tabIds = tabs.map((tab) => tab.id);
  const placementIds = tabs.flatMap((tab) => savedIds(tab.root));
  return (
    new Set(tabIds).size === tabIds.length && new Set(placementIds).size === placementIds.length
  );
}

function parseTab(value: unknown): SavedTab[] {
  if (!isRecord(value) || !isIdentity(value.id)) return [];
  if (value.focusedId !== undefined && typeof value.focusedId !== "string") return [];
  const root = parseNode(value.root, 0);
  if (!root) return [];
  const tab: SavedTab = {
    id: value.id,
    root,
    focusedTerminal: typeof value.focusedTerminal === "string" ? value.focusedTerminal : "",
  };
  if (typeof value.title === "string" && value.title.length > 0) tab.title = value.title;
  if (typeof value.focusedId === "string") tab.focusedId = value.focusedId;
  return [tab];
}

const MAX_DEPTH = 16;

function parseNode(value: unknown, depth: number): SavedNode | undefined {
  if (!isRecord(value) || depth > MAX_DEPTH) return undefined;
  if (value.kind === "leaf") return parseLeaf(value);
  return value.kind === "split" ? parseSplit(value, depth) : undefined;
}

function parseSplit(value: Record<string, unknown>, depth: number): SavedNode | undefined {
  if (value.axis !== "row" && value.axis !== "column") return undefined;
  const first = parseNode(value.first, depth + 1);
  const second = parseNode(value.second, depth + 1);
  if (!first || !second) return undefined;
  const ratio = value.ratio === undefined ? 0.5 : value.ratio;
  if (typeof ratio !== "number" || !Number.isFinite(ratio) || ratio <= 0 || ratio >= 1)
    return undefined;
  return { kind: "split", axis: value.axis, ratio, first, second };
}

function parseLeaf(value: Record<string, unknown>): SavedNode | undefined {
  if (!isIdentity(value.terminalId)) return undefined;
  if (value.id !== undefined && !isIdentity(value.id)) return undefined;
  const node: SavedNode = { kind: "leaf", terminalId: value.terminalId };
  if (typeof value.id === "string") node.id = value.id;
  return node;
}

function migrateTab(value: unknown): SavedTab[] {
  if (!isRecord(value) || !isIdentity(value.id) || !Array.isArray(value.terminals)) {
    return [];
  }
  const terminals = value.terminals.filter(isIdentity);
  if (terminals.length !== value.terminals.length) return [];
  const root = terminals
    .map((terminalId): SavedNode => ({ kind: "leaf", terminalId }))
    .reduceRight<SavedNode | undefined>(
      (rest, node, index) =>
        rest
          ? {
              kind: "split",
              axis: "row",
              ratio: 1 / (terminals.length - index),
              first: node,
              second: rest,
            }
          : node,
      undefined,
    );
  if (!root) return [];
  return [
    {
      id: value.id,
      root,
      focusedTerminal: typeof value.focusedTerminal === "string" ? value.focusedTerminal : "",
    },
  ];
}

function isIdentity(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

export function parseDisplay(value: unknown): DisplayPrefs {
  if (!isRecord(value)) return defaultDisplay;
  return sanitizeDisplay({
    fontFamily: typeof value.fontFamily === "string" ? value.fontFamily : defaultDisplay.fontFamily,
    fontSize: typeof value.fontSize === "number" ? value.fontSize : defaultDisplay.fontSize,
    lineHeight: typeof value.lineHeight === "number" ? value.lineHeight : defaultDisplay.lineHeight,
    optionAsAlt: value.optionAsAlt === true,
    themeId: typeof value.themeId === "string" ? value.themeId : defaultDisplay.themeId,
    sidebarVisible: value.sidebarVisible !== false,
    sidebarWidth:
      typeof value.sidebarWidth === "number" ? value.sidebarWidth : defaultDisplay.sidebarWidth,
    notifications: value.notifications !== false,
    cellWidth: typeof value.cellWidth === "number" ? value.cellWidth : defaultDisplay.cellWidth,
    cellHeight: typeof value.cellHeight === "number" ? value.cellHeight : defaultDisplay.cellHeight,
    paddingX: typeof value.paddingX === "number" ? value.paddingX : defaultDisplay.paddingX,
    paddingY: typeof value.paddingY === "number" ? value.paddingY : defaultDisplay.paddingY,
    unfocusedOpacity:
      typeof value.unfocusedOpacity === "number"
        ? value.unfocusedOpacity
        : defaultDisplay.unfocusedOpacity,
    ghosttyKeys: value.ghosttyKeys !== false,
  });
}

export function sanitizeDisplay(display: DisplayPrefs): DisplayPrefs {
  return {
    fontFamily: display.fontFamily.trim().slice(0, 64) || defaultDisplay.fontFamily,
    fontSize: clamp(Math.round(finite(display.fontSize, defaultDisplay.fontSize)), 9, 28),
    lineHeight: clamp(
      Math.round(finite(display.lineHeight, defaultDisplay.lineHeight) * 20) / 20,
      1,
      2,
    ),
    optionAsAlt: display.optionAsAlt,
    // "ghostty" is resolved at runtime against the user's config.
    themeId:
      display.themeId === "ghostty" || themes.some((theme) => theme.id === display.themeId)
        ? display.themeId
        : defaultDisplay.themeId,
    sidebarVisible: display.sidebarVisible,
    sidebarWidth: clamp(
      Math.round(finite(display.sidebarWidth, defaultDisplay.sidebarWidth)),
      SIDEBAR_MIN,
      SIDEBAR_MAX,
    ),
    notifications: display.notifications,
    cellWidth: clamp(roundTo(finite(display.cellWidth, 1), 100), 0.5, 2),
    cellHeight: clamp(roundTo(finite(display.cellHeight, 1), 100), 0.5, 2),
    paddingX: clamp(Math.round(finite(display.paddingX, defaultDisplay.paddingX)), 0, 64),
    paddingY: clamp(Math.round(finite(display.paddingY, defaultDisplay.paddingY)), 0, 64),
    unfocusedOpacity: clamp(roundTo(finite(display.unfocusedOpacity, 1), 100), 0.15, 1),
    ghosttyKeys: display.ghosttyKeys,
  };
}

function roundTo(value: number, steps: number): number {
  return Math.round(value * steps) / steps;
}

function finite(value: number, fallback: number): number {
  return Number.isFinite(value) ? value : fallback;
}

function clamp(value: number, low: number, high: number): number {
  return Math.min(high, Math.max(low, value));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
