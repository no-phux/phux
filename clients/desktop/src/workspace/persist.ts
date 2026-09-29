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
}

export const defaultDisplay: DisplayPrefs = {
  fontFamily: "Menlo",
  fontSize: 13,
  lineHeight: 1.3,
  optionAsAlt: false,
  themeId: defaultThemeId,
  sidebarVisible: true,
  sidebarWidth: 248,
  notifications: true,
};

export const fontFamilies = [
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
  | { kind: "leaf"; terminalId: string }
  | { kind: "split"; axis: Axis; ratio: number; first: SavedNode; second: SavedNode };

export interface SavedTab {
  id: string;
  title?: string;
  root: SavedNode;
  focusedTerminal: string;
}

export interface SavedLayout {
  version: 2;
  serverId: string;
  activeTab?: string;
  tabs: SavedTab[];
  display: DisplayPrefs;
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
      };
      if (tab.title) saved.title = tab.title;
      return saved;
    }),
  };
  if (activeTab) layout.activeTab = activeTab;
  return layout;
}

function saveNode(node: LayoutNode): SavedNode {
  if (node.kind === "leaf") return { kind: "leaf", terminalId: node.placement.terminalId };
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
  if (!tabs) return undefined;
  const layout: SavedLayout = {
    version: 2,
    serverId: value.serverId,
    tabs,
    display: parseDisplay(value.display),
  };
  if (typeof value.activeTab === "string") layout.activeTab = value.activeTab;
  return layout;
}

function parseTab(value: unknown): SavedTab[] {
  if (!isRecord(value) || typeof value.id !== "string") return [];
  const root = parseNode(value.root, 0);
  if (!root) return [];
  const tab: SavedTab = {
    id: value.id,
    root,
    focusedTerminal: typeof value.focusedTerminal === "string" ? value.focusedTerminal : "",
  };
  if (typeof value.title === "string" && value.title.length > 0) tab.title = value.title;
  return [tab];
}

const MAX_DEPTH = 16;

function parseNode(value: unknown, depth: number): SavedNode | undefined {
  if (!isRecord(value) || depth > MAX_DEPTH) return undefined;
  if (value.kind === "leaf") {
    return typeof value.terminalId === "string"
      ? { kind: "leaf", terminalId: value.terminalId }
      : undefined;
  }
  if (value.kind !== "split" || (value.axis !== "row" && value.axis !== "column")) return undefined;
  const first = parseNode(value.first, depth + 1);
  const second = parseNode(value.second, depth + 1);
  if (!first || !second) return first ?? second;
  const ratio = typeof value.ratio === "number" ? value.ratio : 0.5;
  return { kind: "split", axis: value.axis, ratio, first, second };
}

function migrateTab(value: unknown): SavedTab[] {
  if (!isRecord(value) || typeof value.id !== "string" || !Array.isArray(value.terminals)) {
    return [];
  }
  const terminals = value.terminals.filter((item): item is string => typeof item === "string");
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
    themeId: themes.some((theme) => theme.id === display.themeId)
      ? display.themeId
      : defaultDisplay.themeId,
    sidebarVisible: display.sidebarVisible,
    sidebarWidth: clamp(
      Math.round(finite(display.sidebarWidth, defaultDisplay.sidebarWidth)),
      SIDEBAR_MIN,
      SIDEBAR_MAX,
    ),
    notifications: display.notifications,
  };
}

function finite(value: number, fallback: number): number {
  return Number.isFinite(value) ? value : fallback;
}

function clamp(value: number, low: number, high: number): number {
  return Math.min(high, Math.max(low, value));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
