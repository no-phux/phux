export interface Placement {
  id: string;
  terminalId: string;
  viewId: string;
}

export interface DeskTab {
  id: string;
  title: string;
  placements: Placement[];
  focusedId: string;
}

export interface DisplayPrefs {
  fontSize: number;
  optionAsAlt: boolean;
}

export const defaultDisplay: DisplayPrefs = { fontSize: 14, optionAsAlt: false };

export interface SavedLayout {
  version: 1;
  serverId: string;
  tabs: Array<{ id: string; title: string; terminals: string[]; focusedTerminal: string }>;
  display: DisplayPrefs;
}

let nextId = 1;

export function newId(prefix: string): string {
  nextId += 1;
  return `${prefix}-${nextId}`;
}

export function focusedPlacement(tab: DeskTab): Placement | undefined {
  return tab.placements.find((placement) => placement.id === tab.focusedId) ?? tab.placements[0];
}

export function saveLayout(
  serverId: string,
  tabs: DeskTab[],
  display: DisplayPrefs = defaultDisplay,
): SavedLayout {
  return {
    version: 1,
    serverId,
    display: sanitizeDisplay(display),
    tabs: tabs.map((tab) => ({
      id: tab.id,
      title: tab.title,
      terminals: tab.placements.map((placement) => placement.terminalId),
      focusedTerminal: focusedPlacement(tab)?.terminalId ?? "",
    })),
  };
}

export function parseLayout(value: unknown): SavedLayout | undefined {
  if (!isRecord(value) || value.version !== 1 || typeof value.serverId !== "string")
    return undefined;
  if (!Array.isArray(value.tabs)) return undefined;
  const tabs = value.tabs.flatMap((tab) => {
    if (!isRecord(tab) || typeof tab.id !== "string" || typeof tab.title !== "string") return [];
    if (!Array.isArray(tab.terminals) || typeof tab.focusedTerminal !== "string") return [];
    const terminals = tab.terminals.filter(
      (terminal): terminal is string => typeof terminal === "string",
    );
    return [{ id: tab.id, title: tab.title, terminals, focusedTerminal: tab.focusedTerminal }];
  });
  return { version: 1, serverId: value.serverId, tabs, display: parseDisplay(value.display) };
}

function parseDisplay(value: unknown): DisplayPrefs {
  if (!isRecord(value)) return defaultDisplay;
  return sanitizeDisplay({
    fontSize: typeof value.fontSize === "number" ? value.fontSize : defaultDisplay.fontSize,
    optionAsAlt: value.optionAsAlt === true,
  });
}

function sanitizeDisplay(display: DisplayPrefs): DisplayPrefs {
  const size = Number.isFinite(display.fontSize) ? Math.round(display.fontSize) : 14;
  return {
    fontSize: Math.min(28, Math.max(10, size)),
    optionAsAlt: display.optionAsAlt,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
