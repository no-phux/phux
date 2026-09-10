import { Cmd, asciiBytes, utf8Bytes, windowDescriptor } from "@native-sdk/core";
import { type WindowDescriptor } from "@native-sdk/core/events";
import { applyTextInputEvent, type TextEditState, type TextInputEvent } from "@native-sdk/core/text";
import { type TabCommandState, type TabCommandDecision, initialTabCommands, enqueueTabCommand, enqueueCatalogCommand, enqueueOperationCommand, receiveTabReceipt, unknownTabCommand } from "./tab-commands.ts";
import { type CommandResults, type ResultDecision, initialCommandResults, requestCommandResults, receiveCommandResult, failedCommandResults } from "./command-results.ts";
import {
  ENGINE_CHANNEL_KEY,
  type WireU64,
  type SnapshotTab,
  type SecondaryWindow,
  invalidation,
  intent,
  sameU64,
  snapshot,
  navigationRequest,
  navigationPage,
  sameBytes,
  type SnapshotAgentRow,
} from "./protocol.ts";

/// One agent session drawn under the terminal tab it runs in. It is not a
/// tab: it carries no slot, takes no selection, and owns no pane. `id` is its
/// order under that tab, which is all a sibling-scoped `key` needs.
export interface AgentRow {
  readonly id: number;
  readonly provider: Uint8Array;
  readonly state: Uint8Array;
  readonly attention: boolean;
}

/// One drawn row of the side rail: a terminal tab, or an agent session
/// indented under the tab above it. `agent` picks the shape. A terminal row
/// captures its tab target; an agent summary has no selection handler.
export interface RailRow {
  readonly id: number;
  readonly index: number;
  readonly label: Uint8Array;
  readonly state: Uint8Array;
  readonly mark: Uint8Array;
  readonly selected: boolean;
  readonly agent: boolean;
  readonly target: Uint8Array;
}

export interface Tab {
  readonly id: number;
  readonly index: number;
  readonly slot: number;
  readonly title: Uint8Array;
  readonly cwd: Uint8Array;
  readonly selected: boolean;
  readonly attention: boolean;
  /// Live, never persisted: the rows come from the snapshot the engine just
  /// sent, so a session that closed is simply absent from the next one.
  readonly agents: readonly AgentRow[];
  readonly target: Uint8Array;
}

/// One catalog row: display bookkeeping and label beside the captured native
/// target. Only those opaque bytes authorize selection.
export interface SwitcherRow {
  readonly id: number;
  readonly index: number;
  readonly label: Uint8Array;
  readonly target: Uint8Array;
  readonly highlighted: boolean;
}

export interface ThemeRow {
  readonly index: number;
  readonly label: Uint8Array;
  readonly active: boolean;
  readonly highlighted: boolean;
}

/// One secondary window slot, as the engine's snapshot reports it. `open`
/// is presence in the snapshot; a closed slot keeps empty lists so the
/// slot's markup always has something to bind. `slot` on a tab packs the
/// window and the index into one number for the markup's single-value
/// handlers (window * 32 + index; a workspace holds at most sixteen tabs).
export interface WindowState {
  readonly index: number;
  readonly open: boolean;
  readonly tabs: readonly Tab[];
  readonly visibleTabs: readonly Tab[];
  readonly tabWidth: number;
  readonly hasOverflow: boolean;
  readonly overflowLabel: Uint8Array;
  readonly selectedTab: number;
}

export type TabPlacement = "top" | "side";
export type ChannelState = "data" | "closed" | "rejected";

export interface Model {
  readonly tabs: readonly Tab[];
  /// The run the band has room for, always holding the selected tab. The
  /// toolkit cannot bound a run by itself, and the core cannot measure a
  /// band; the engine's snapshot carries the shipping projection's answer
  /// and the core slices to it, with a cue for the rest.
  readonly visibleTabs: readonly Tab[];
  readonly tabWidth: number;
  readonly hasOverflow: boolean;
  readonly overflowLabel: Uint8Array;
  readonly selectedTab: number;
  readonly tabPlacement: TabPlacement;
  /// The side rail, flattened: each visible tab followed by the agent rows
  /// running inside it. The compiled markup iterates model slices, not a
  /// field of a `for` item, so the nesting is expressed here as order and
  /// depth rather than as a loop inside a loop.
  readonly railRows: readonly RailRow[];
  /// Native projection slot for the platform window that owns modal chrome.
  /// Platform ids never cross the seam; the snapshot carries only 0..4.
  readonly activeWindow: number;
  readonly paletteOpen: boolean;
  readonly mainPaletteOpen: boolean;
  readonly paletteQuery: Uint8Array;
  readonly paletteAnchor: number;
  readonly paletteFocus: number;
  readonly paletteRows: readonly SwitcherRow[];
  readonly paletteCursor: number;
  readonly paletteOffset: number;
  readonly paletteTotal: number;
  readonly palettePrevious: boolean;
  readonly paletteNext: boolean;
  readonly paletteLoading: boolean;
  readonly paletteNotice: Uint8Array;
  readonly canReconnect: boolean;
  readonly connectionStatus: Uint8Array;
  readonly window1Status: Uint8Array;
  readonly window2Status: Uint8Array;
  readonly window3Status: Uint8Array;
  readonly window4Status: Uint8Array;
  readonly settingsOpen: boolean;
  readonly mainSettingsOpen: boolean;
  readonly themes: readonly ThemeRow[];
  readonly settingsCursor: number;
  readonly configExists: boolean;
  readonly configNotice: Uint8Array;
  // Each secondary slot flattened: a markup template takes scalars and
  // slices as arguments, not records, so the slot's markup binds these.
  readonly window1Open: boolean;
  readonly window1Tabs: readonly Tab[];
  readonly window1TabWidth: number;
  readonly window1HasOverflow: boolean;
  readonly window1OverflowLabel: Uint8Array;
  readonly window1PaletteOpen: boolean;
  readonly window1SettingsOpen: boolean;
  readonly window2Open: boolean;
  readonly window2Tabs: readonly Tab[];
  readonly window2TabWidth: number;
  readonly window2HasOverflow: boolean;
  readonly window2OverflowLabel: Uint8Array;
  readonly window2PaletteOpen: boolean;
  readonly window2SettingsOpen: boolean;
  readonly window3Open: boolean;
  readonly window3Tabs: readonly Tab[];
  readonly window3TabWidth: number;
  readonly window3HasOverflow: boolean;
  readonly window3OverflowLabel: Uint8Array;
  readonly window3PaletteOpen: boolean;
  readonly window3SettingsOpen: boolean;
  readonly window4Open: boolean;
  readonly window4Tabs: readonly Tab[];
  readonly window4TabWidth: number;
  readonly window4HasOverflow: boolean;
  readonly window4OverflowLabel: Uint8Array;
  readonly window4PaletteOpen: boolean;
  readonly window4SettingsOpen: boolean;
  readonly engineConnected: boolean;
  readonly engineSequence: WireU64;
  readonly engineRevision: WireU64;
  readonly tabCommands: TabCommandState;
  readonly commandResults: CommandResults;
  readonly commandNotice: Uint8Array;
  readonly status: Uint8Array;
}

export type Msg =
  | { readonly kind: "command_result_loaded"; readonly body: Uint8Array }
  | { readonly kind: "command_result_failed"; readonly error: Uint8Array }
  | { readonly kind: "select_target"; readonly target: Uint8Array }
  | { readonly kind: "tab_command_completed"; readonly body: Uint8Array }
  | { readonly kind: "tab_command_failed"; readonly error: Uint8Array }
  | { readonly kind: "select_tab"; readonly index: number }
  | { readonly kind: "select_active_tab"; readonly index: number }
  | { readonly kind: "select_slot"; readonly slot: number }
  | { readonly kind: "new_terminal" }
  | { readonly kind: "new_window" }
  | { readonly kind: "window_closed"; readonly window: number }
  | { readonly kind: "close_selected_tab" }
  | { readonly kind: "toggle_tab_placement" }
  | { readonly kind: "palette_open" }
  | { readonly kind: "palette_close" }
  | { readonly kind: "palette_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "palette_move"; readonly delta: number }
  | { readonly kind: "palette_submit" }
  | { readonly kind: "palette_pick"; readonly target: Uint8Array }
  | { readonly kind: "palette_previous" }
  | { readonly kind: "palette_next" }
  | { readonly kind: "palette_retry" }
  | { readonly kind: "navigation_loaded"; readonly body: Uint8Array }
  | { readonly kind: "navigation_failed"; readonly error: Uint8Array }
  | { readonly kind: "reconnect" }
  | { readonly kind: "settings_open" }
  | { readonly kind: "settings_close" }
  | { readonly kind: "settings_move"; readonly delta: number }
  | { readonly kind: "settings_pick"; readonly index: number }
  | { readonly kind: "settings_commit" }
  | { readonly kind: "settings_reveal" }
  | { readonly kind: "native_command"; readonly command: number }
  // Posted by the native engine for every shell event it consumed: no bytes
  // ride along, the core only learns that the grids beneath it moved.
  | { readonly kind: "engine_wake" }
  | { readonly kind: "snapshot_loaded"; readonly body: Uint8Array }
  | { readonly kind: "snapshot_failed"; readonly error: Uint8Array }
  | {
      readonly kind: "engine_event";
      readonly key: number;
      readonly state: ChannelState;
      readonly bytes: Uint8Array;
      readonly droppedPending: number;
      readonly droppedTotal: number;
    };

export const viewUnbound = [
  "select_tab",
  "select_slot",
  "tabCommands",
  "commandResults",
  "command_result_loaded",
  "command_result_failed",
  "tab_command_completed",
  "tab_command_failed",
  "selectedTab",
  "activeWindow",
  "paletteOpen",
  "settingsOpen",
  "select_active_tab",
  "window_closed",
  "paletteAnchor",
  "paletteFocus",
  "paletteCursor",
  "paletteOffset",
  "paletteTotal",
  "paletteLoading",
  "navigation_loaded",
  "navigation_failed",
  "settingsCursor",
  "palette_move",
  "settings_move",
  "engineConnected",
  "engineSequence",
  "engineRevision",
  "status",
  "engine_event",
  "engine_wake",
  "snapshot_loaded",
  "snapshot_failed",
  "native_command",
] as const;

const ZERO_U64: WireU64 = { hi: 0, lo: 0 };

function overflowLabel(hidden: number): Uint8Array {
  // "+N" for N in 1..255 without string building or division, neither of
  // which the compiled subset offers on an integer path: peel hundreds and
  // tens by subtraction.
  let rest = hidden;
  let hundreds = 0;
  while (rest >= 100) {
    rest -= 100;
    hundreds += 1;
  }
  let tens = 0;
  while (rest >= 10) {
    rest -= 10;
    tens += 1;
  }
  const digits = hundreds > 0 ? 3 : tens > 0 ? 2 : 1;
  const out = new Uint8Array(1 + digits);
  out[0] = 43;
  let at = 1;
  if (hundreds > 0) {
    out[at] = 48 + hundreds;
    at += 1;
  }
  if (hundreds > 0 || tens > 0) {
    out[at] = 48 + tens;
    at += 1;
  }
  out[at] = 48 + rest;
  return out;
}

function joinBytes(head: Uint8Array, mid: Uint8Array, tail: Uint8Array): Uint8Array {
  const out = new Uint8Array(head.length + mid.length + tail.length);
  let at = 0;
  for (let i = 0; i < head.length; i += 1) {
    out[at] = head[i];
    at += 1;
  }
  for (let i = 0; i < mid.length; i += 1) {
    out[at] = mid[i];
    at += 1;
  }
  for (let i = 0; i < tail.length; i += 1) {
    out[at] = tail[i];
    at += 1;
  }
  return out;
}

const NO_BYTES = new Uint8Array(0);

/// U+25CF BLACK CIRCLE. Deliberately the same quiet dot the tab strip uses
/// for a terminal asking for attention: an agent waiting on an answer is the
/// same claim on a person, not a louder one.
const ATTENTION_MARK = utf8Bytes("\u25cf");
// Typed empties: a bare `[]` in a record literal is inferred as number[] and
// the record then fails to be a Model at the union boundary, at runtime.
const NO_ROWS: readonly SwitcherRow[] = [];
const NO_TABS: readonly Tab[] = [];
const NO_THEMES: readonly ThemeRow[] = [];

function paletteState(model: Model): TextEditState {
  return {
    text: model.paletteQuery,
    selection: { anchor: model.paletteAnchor, focus: model.paletteFocus },
    composition: null,
  };
}

function requestNavigation(model: Model, offset: number): Model {
  const at = offset >= 0 && offset <= 65535 ? Math.trunc(offset) : 0;
  return { ...model, paletteOffset: at, paletteRows: NO_ROWS, paletteCursor: 0, paletteLoading: true,
    palettePrevious: false, paletteNext: false, paletteNotice: asciiBytes("Loading workspace...") };
}

function closePalette(model: Model): Model {
  return scopeOverlays({ ...model, paletteOpen: false, paletteQuery: NO_BYTES, paletteRows: NO_ROWS, paletteCursor: 0 });
}

function refreshNavigation(model: Model): Model {
  const refreshed = requestNavigation(model, model.paletteOffset);
  return { ...refreshed, paletteCursor: model.paletteCursor };
}

function navigationTarget(model: Model, msg: Msg): Uint8Array {
  if (!model.paletteOpen) return NO_BYTES;
  if (msg.kind === "palette_pick") return msg.target;
  if (model.paletteLoading || model.paletteRows.length === 0) return NO_BYTES;
  return model.paletteRows[model.paletteCursor].target;
}

function loadedNavigation(model: Model, body: Uint8Array): Model {
  if (!model.paletteOpen || !model.engineConnected) return model;
  const page = navigationPage(body);
  if (page === null) return { ...model, paletteLoading: false, paletteNotice: asciiBytes("Workspace unavailable. Retry to refresh.") };
  if (!sameU64(page.revision, model.engineRevision)) return model;
  if (page.offset !== model.paletteOffset || !sameBytes(page.query, model.paletteQuery)) return model;
  const total = page.total >= 0 && page.total <= 65535 ? Math.trunc(page.total) : 0;
  const loaded: Model = { ...model, paletteRows: page.rows, paletteTotal: total, paletteLoading: false,
    palettePrevious: page.offset > 0, paletteNext: page.offset + page.rows.length < total,
    paletteNotice: total === 0 ? asciiBytes("No matching terminals or sessions") : asciiBytes("Open panes / Available terminals / Sessions") };
  return highlightNavigation(loaded, Math.min(model.paletteCursor, page.rows.length - 1));
}

function moveNavigation(model: Model, delta: number): Model {
  if (!model.paletteOpen || model.paletteLoading) return model;
  const next = model.paletteCursor + (delta >= 0 ? 1 : -1);
  if (next < 0 && model.palettePrevious) return previousNavigation(model);
  if (next >= model.paletteRows.length && model.paletteNext) return requestNavigation(model, model.paletteOffset + 4);
  return highlightNavigation(model, next);
}

function previousNavigation(model: Model): Model {
  const previous = requestNavigation(model, model.paletteOffset - 4);
  return { ...previous, paletteCursor: 3 };
}

function highlightNavigation(model: Model, next: number): Model {
  if (next < 0 || next >= model.paletteRows.length) return model;
  const cursor = next >= 0 && next <= 3 ? Math.trunc(next) : 0;
  const rows: SwitcherRow[] = [];
  for (let i = 0; i < model.paletteRows.length; i += 1) {
    const row = model.paletteRows[i];
    rows.push({ ...row, highlighted: i === cursor });
  }
  return { ...model, paletteCursor: cursor, paletteRows: rows };
}

function editNavigation(model: Model, edit: TextInputEvent): Model {
  if (!model.paletteOpen) return model;
  const next = applyTextInputEvent(paletteState(model), edit, 64);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 64 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 64 ? Math.trunc(next.selection.focus) : 0;
  return requestNavigation({ ...model, paletteQuery: next.text, paletteAnchor: anchor, paletteFocus: focus }, 0);
}

function browseNavigation(model: Model, msg: Msg): Model {
  if (!model.paletteOpen) return model;
  switch (msg.kind) {
    case "palette_previous": return model.palettePrevious ? requestNavigation(model, model.paletteOffset - 4) : model;
    case "palette_next": return model.paletteNext ? requestNavigation(model, model.paletteOffset + 4) : model;
    case "palette_retry": return requestNavigation(model, 0);
    default: return model;
  }
}

function changeNavigation(model: Model, msg: Msg): Model {
  switch (msg.kind) {
    case "palette_open":
      if (model.paletteOpen) return model;
      return requestNavigation(scopeOverlays({ ...model, paletteOpen: true, settingsOpen: false, paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0 }), 0);
    case "palette_edit": return editNavigation(model, msg.edit);
    case "palette_move": return moveNavigation(model, msg.delta);
    default: return browseNavigation(model, msg);
  }
}

function connectionLabel(state: number): Uint8Array {
  if (state === 0) return asciiBytes("Local terminals");
  if (state === 1) return asciiBytes("Phux connecting...");
  if (state === 2) return asciiBytes("Phux connected");
  if (state === 4) return asciiBytes("Phux connected / Shared workspace unavailable");
  return asciiBytes("Phux offline");
}

function terminalStateLabel(state: number): Uint8Array {
  if (state === 1) return asciiBytes("Loading terminal");
  if (state === 2) return asciiBytes("Recovering terminal: waiting for snapshot");
  if (state === 3) return asciiBytes("Terminal frozen: waiting for recovery");
  if (state === 4) return asciiBytes("Terminal unavailable");
  if (state === 5) return asciiBytes("Terminal ended");
  if (state === 6) return asciiBytes("Loading earlier history");
  if (state === 7) return asciiBytes("Earlier history available");
  return NO_BYTES;
}

function windowStatus(connection: number, terminal: number, refused: boolean): Uint8Array {
  const global = refused ? joinBytes(connectionLabel(connection), asciiBytes(" / Action refused"), NO_BYTES) : connectionLabel(connection);
  if (terminal === 0) return global;
  return joinBytes(global, asciiBytes(" / "), terminalStateLabel(terminal));
}

function engineUnavailable(model: Model, status: Uint8Array): Model {
  return { ...model, engineConnected: false, status, canReconnect: false,
    connectionStatus: asciiBytes("Connection status unavailable"), paletteRows: NO_ROWS,
    window1Status: asciiBytes("Connection status unavailable"),
    window2Status: asciiBytes("Connection status unavailable"),
    window3Status: asciiBytes("Connection status unavailable"),
    window4Status: asciiBytes("Connection status unavailable"),
    paletteLoading: false, palettePrevious: false, paletteNext: false };
}

/// Only the active native window presents the global core-owned modal. The
/// booleans are flattened because `.native` template arguments bind fields,
/// not comparisons; `paletteOpen`/`settingsOpen` remain the keyboard gate.
function scopeOverlays(model: Model): Model {
  const active = model.activeWindow >= 0 && model.activeWindow <= 4 ? Math.trunc(model.activeWindow) : 0;
  return {
    ...model,
    mainPaletteOpen: model.paletteOpen && active === 0,
    mainSettingsOpen: model.settingsOpen && active === 0,
    window1PaletteOpen: model.paletteOpen && active === 1,
    window1SettingsOpen: model.settingsOpen && active === 1,
    window2PaletteOpen: model.paletteOpen && active === 2,
    window2SettingsOpen: model.settingsOpen && active === 2,
    window3PaletteOpen: model.paletteOpen && active === 3,
    window3SettingsOpen: model.settingsOpen && active === 3,
    window4PaletteOpen: model.paletteOpen && active === 4,
    window4SettingsOpen: model.settingsOpen && active === 4,
  };
}

const IN_EFFECT = asciiBytes("  (in effect)");

function themeRows(themes: readonly { readonly index: number; readonly name: Uint8Array }[], active: number, cursor: number): readonly ThemeRow[] {
  return themes.map((theme) => {
    const index = theme.index >= 0 && theme.index <= 32 ? Math.trunc(theme.index) : 0;
    return {
      index,
      label: index === active ? joinBytes(theme.name, IN_EFFECT, NO_BYTES) : theme.name,
      active: index === active,
      highlighted: index === cursor,
    };
  });
}

/// Re-highlight the catalog at `cursor`. A loop rather than a map with a
/// captured cursor: the compiled subset loses an integer proof across the
/// capture, and the cursor also lands in an integer slot.
function highlightThemes(themes: readonly ThemeRow[], cursor: number): readonly ThemeRow[] {
  const out: ThemeRow[] = [];
  for (let i = 0; i < themes.length; i += 1) {
    const t = themes[i];
    out.push({ index: t.index, label: t.label, active: t.active, highlighted: t.index === cursor });
  }
  return out;
}

/// The Configuration line the shipping settings panel shows, from the
/// engine's probe. Same sentences, same conditions (view.zig settingsPanel).
function configNotice(enabled: boolean, probed: boolean, exists: boolean, writable: boolean, refused: boolean, path: Uint8Array): Uint8Array {
  if (!enabled) return asciiBytes("No config location could be resolved; changes apply to this run only.");
  if (!probed) return joinBytes(asciiBytes("Active configuration file: "), path, NO_BYTES);
  if (refused) return joinBytes(path, asciiBytes(" refused the write; changes apply to this run only."), NO_BYTES);
  if (!writable) return joinBytes(path, asciiBytes(" is read-only; changes apply to this run only."), NO_BYTES);
  if (!exists) return joinBytes(path, asciiBytes(" will be created when you save."), NO_BYTES);
  return joinBytes(asciiBytes("Active configuration file: "), path, NO_BYTES);
}

/// The display word for each `agent_sessions.State`, in wire order. The
/// engine sends the ordinal and the core says the word: the vocabulary is
/// closed, so shipping the text per row would be bytes for nothing.
const AGENT_STATE_WORDS: readonly Uint8Array[] = [
  asciiBytes("unknown"),
  asciiBytes("working"),
  asciiBytes("blocked"),
  asciiBytes("done"),
  asciiBytes("gone"),
];

const NO_AGENT_ROWS: readonly AgentRow[] = [];
const NO_RAIL_ROWS: readonly RailRow[] = [];

/// The agent rows this window's tab owns, in the order the snapshot listed
/// them. A row whose state ordinal is outside the closed vocabulary is
/// dropped rather than shown as a word the engine never said.
function agentRowsFor(agents: readonly SnapshotAgentRow[], window: number, tab: number): readonly AgentRow[] {
  const out: AgentRow[] = [];
  let ordinal = 0;
  for (let i = 0; i < agents.length; i += 1) {
    const row = agents[i];
    if (row.window !== window || row.tab !== tab) continue;
    const state = row.state;
    if (!(state >= 0 && state < AGENT_STATE_WORDS.length)) continue;
    if (!(ordinal >= 0 && ordinal <= 255)) continue;
    out.push({
      id: Math.trunc(ordinal),
      provider: row.provider,
      state: AGENT_STATE_WORDS[Math.trunc(state)],
      attention: row.attention,
    });
    ordinal += 1;
  }
  return out.length === 0 ? NO_AGENT_ROWS : out;
}

function stampSlots(tabs: readonly SnapshotTab[], window: number, agents: readonly SnapshotAgentRow[]): readonly Tab[] {
  const out: Tab[] = [];
  const w = window >= 0 && window <= 4 ? Math.trunc(window) : 0;
  for (let i = 0; i < tabs.length; i += 1) {
    const t = tabs[i];
    const rawIndex = t.index;
    const rawId = t.id;
    if (!(rawIndex >= 0 && rawIndex <= 31) || !(rawId >= 1 && rawId <= 4294967295)) continue;
    const index = Math.trunc(rawIndex);
    const id = Math.trunc(rawId);
    out.push({ id, index, slot: w * 32 + index, title: t.title, cwd: t.cwd, selected: t.selected, attention: t.attention, agents: agentRowsFor(agents, w, index), target: t.target });
  }
  return out;
}

/// The rail: each visible tab, then its agent rows in catalog order. A row
/// that is no longer in the snapshot is simply not built, which is the whole
/// of "the row goes away when the session closes".
function railRows(tabs: readonly Tab[]): readonly RailRow[] {
  const out: RailRow[] = [];
  let ordinal = 0;
  for (let i = 0; i < tabs.length; i += 1) {
    const tab = tabs[i];
    if (!(ordinal >= 0 && ordinal <= 65535)) break;
    out.push({ id: Math.trunc(ordinal), index: tab.index, label: tab.title, state: NO_BYTES, mark: NO_BYTES, selected: tab.selected, agent: false, target: tab.target });
    ordinal += 1;
    const rows = tab.agents;
    for (let j = 0; j < rows.length; j += 1) {
      const row = rows[j];
      if (!(ordinal >= 0 && ordinal <= 65535)) break;
      out.push({ id: Math.trunc(ordinal), index: tab.index, label: row.provider, state: row.state, mark: row.attention ? ATTENTION_MARK : NO_BYTES, selected: false, agent: true, target: NO_BYTES });
      ordinal += 1;
    }
  }
  return out;
}

const CLOSED_WINDOW: WindowState = {
  index: 0,
  open: false,
  tabs: [],
  visibleTabs: [],
  tabWidth: 168,
  hasOverflow: false,
  overflowLabel: new Uint8Array(0),
  selectedTab: 0,
};

function closedWindow(index: number): WindowState {
  const at = index >= 0 && index <= 4 ? Math.trunc(index) : 0;
  return { ...CLOSED_WINDOW, index: at };
}

function windowState(index: number, section: SecondaryWindow | null, agents: readonly SnapshotAgentRow[]): WindowState {
  if (section === null) return closedWindow(index);
  const at = index >= 0 && index <= 4 ? Math.trunc(index) : 0;
  const tabs = stampSlots(section.tabs, at, agents);
  const hidden = tabs.length - section.runCount;
  const selected = section.selectedTab >= 0 && section.selectedTab <= 255 ? Math.trunc(section.selectedTab) : 0;
  const width = section.tabWidth >= 0 && section.tabWidth <= 65535 ? Math.trunc(section.tabWidth) : 168;
  return {
    index: at,
    open: true,
    tabs,
    visibleTabs: sliceRun(tabs, section.runStart, section.runCount),
    tabWidth: width,
    hasOverflow: hidden > 0,
    overflowLabel: hidden > 0 ? overflowLabel(hidden) : new Uint8Array(0),
    selectedTab: selected,
  };
}

// Each slot's descriptor spells its labels literally: the compiled subset
// binds `src/windows/<label>.native` to the literal at build time.
function describeWindow1(): WindowDescriptor {
  return windowDescriptor({
    label: asciiBytes("phux-window-1"),
    canvasLabel: asciiBytes("phux-cockpit-canvas-1"),
    title: asciiBytes("Phux Cockpit TS"),
    width: 1100,
    height: 640,
    minWidth: 900,
    minHeight: 420,
    titlebar: "hidden_inset_tall",
    closePolicy: "quit",
    onCloseCommand: asciiBytes("cockpit.window.closed.1"),
  });
}

function describeWindow2(): WindowDescriptor {
  return windowDescriptor({
    label: asciiBytes("phux-window-2"),
    canvasLabel: asciiBytes("phux-cockpit-canvas-2"),
    title: asciiBytes("Phux Cockpit TS"),
    width: 1100,
    height: 640,
    minWidth: 900,
    minHeight: 420,
    titlebar: "hidden_inset_tall",
    closePolicy: "quit",
    onCloseCommand: asciiBytes("cockpit.window.closed.2"),
  });
}

function describeWindow3(): WindowDescriptor {
  return windowDescriptor({
    label: asciiBytes("phux-window-3"),
    canvasLabel: asciiBytes("phux-cockpit-canvas-3"),
    title: asciiBytes("Phux Cockpit TS"),
    width: 1100,
    height: 640,
    minWidth: 900,
    minHeight: 420,
    titlebar: "hidden_inset_tall",
    closePolicy: "quit",
    onCloseCommand: asciiBytes("cockpit.window.closed.3"),
  });
}

function describeWindow4(): WindowDescriptor {
  return windowDescriptor({
    label: asciiBytes("phux-window-4"),
    canvasLabel: asciiBytes("phux-cockpit-canvas-4"),
    title: asciiBytes("Phux Cockpit TS"),
    width: 1100,
    height: 640,
    minWidth: 900,
    minHeight: 420,
    titlebar: "hidden_inset_tall",
    closePolicy: "quit",
    onCloseCommand: asciiBytes("cockpit.window.closed.4"),
  });
}

/// The secondary windows the engine has open, as platform windows: the same
/// labels the shipping scene declares, so the engine paints, sizes and routes
/// each one through its own table. Presence is liveness. The compiled subset
/// requires an array literal of descriptor calls per return, so every
/// combination of open slots is spelled out.
export function windows(model: Model): readonly WindowDescriptor[] {
  const a = model.window1Open;
  const b = model.window2Open;
  const c = model.window3Open;
  const d = model.window4Open;
  if (a && b && c && d) return [describeWindow1(), describeWindow2(), describeWindow3(), describeWindow4()];
  if (a && b && c && !d) return [describeWindow1(), describeWindow2(), describeWindow3()];
  if (a && b && !c && d) return [describeWindow1(), describeWindow2(), describeWindow4()];
  if (a && b && !c && !d) return [describeWindow1(), describeWindow2()];
  if (a && !b && c && d) return [describeWindow1(), describeWindow3(), describeWindow4()];
  if (a && !b && c && !d) return [describeWindow1(), describeWindow3()];
  if (a && !b && !c && d) return [describeWindow1(), describeWindow4()];
  if (a && !b && !c && !d) return [describeWindow1()];
  if (!a && b && c && d) return [describeWindow2(), describeWindow3(), describeWindow4()];
  if (!a && b && c && !d) return [describeWindow2(), describeWindow3()];
  if (!a && b && !c && d) return [describeWindow2(), describeWindow4()];
  if (!a && b && !c && !d) return [describeWindow2()];
  if (!a && !b && c && d) return [describeWindow3(), describeWindow4()];
  if (!a && !b && c && !d) return [describeWindow3()];
  if (!a && !b && !c && d) return [describeWindow4()];
  return [];
}

/// The OS closed a window: tell the engine, which retires the slot and its
/// shells; the next snapshot drops the window from `windows(model)`.
export function commandMsg(name: string): Msg | null {
  if (name === "cockpit.window.closed.1") return { kind: "window_closed", window: 1 };
  if (name === "cockpit.window.closed.2") return { kind: "window_closed", window: 2 };
  if (name === "cockpit.window.closed.3") return { kind: "window_closed", window: 3 };
  if (name === "cockpit.window.closed.4") return { kind: "window_closed", window: 4 };
  if (name === "surface.1") return { kind: "select_active_tab", index: 0 };
  if (name === "surface.2") return { kind: "select_active_tab", index: 1 };
  if (name === "surface.3") return { kind: "select_active_tab", index: 2 };
  if (name === "surface.4") return { kind: "select_active_tab", index: 3 };
  if (name === "surface.5") return { kind: "select_active_tab", index: 4 };
  if (name === "terminal.new") return { kind: "new_terminal" };
  if (name === "window.new") return { kind: "new_window" };
  if (name === "tabs.palette") return { kind: "palette_open" };
  if (name === "settings.open") return { kind: "settings_open" };
  if (name === "tabs.toggle-placement") return { kind: "toggle_tab_placement" };
  if (name === "tab.previous") return { kind: "native_command", command: 1 };
  if (name === "tab.next") return { kind: "native_command", command: 2 };
  if (name === "terminal.close") return { kind: "native_command", command: 3 };
  if (name === "pane.split-right") return { kind: "native_command", command: 4 };
  if (name === "pane.split-down") return { kind: "native_command", command: 5 };
  if (name === "pane.previous") return { kind: "native_command", command: 6 };
  if (name === "pane.next") return { kind: "native_command", command: 7 };
  if (name === "tab.move-left") return { kind: "native_command", command: 8 };
  if (name === "tab.move-right") return { kind: "native_command", command: 9 };
  if (name === "terminal.select-all") return { kind: "native_command", command: 10 };
  if (name === "terminal.copy") return { kind: "native_command", command: 11 };
  if (name === "terminal.paste") return { kind: "native_command", command: 12 };
  if (name === "terminal.clear") return { kind: "native_command", command: 13 };
  if (name === "terminal.find") return { kind: "native_command", command: 14 };
  if (name === "terminal.find-next") return { kind: "native_command", command: 15 };
  if (name === "terminal.find-previous") return { kind: "native_command", command: 16 };
  if (name === "view.font-larger") return { kind: "native_command", command: 17 };
  if (name === "view.font-smaller") return { kind: "native_command", command: 18 };
  if (name === "view.font-reset") return { kind: "native_command", command: 19 };
  if (name === "pane.focus-left") return { kind: "native_command", command: 20 };
  if (name === "pane.focus-right") return { kind: "native_command", command: 21 };
  if (name === "pane.focus-up") return { kind: "native_command", command: 22 };
  if (name === "pane.focus-down") return { kind: "native_command", command: 23 };
  if (name === "window.fullscreen") return { kind: "native_command", command: 24 };
  if (name === "window.minimize") return { kind: "native_command", command: 25 };
  return null;
}

function findSection(sections: readonly SecondaryWindow[], index: number) {
  for (let i = 0; i < sections.length; i += 1) {
    if (sections[i].index === index) return sections[i];
  }
  return null;
}

/// Slice the tab list to the engine's run. Every index is proven whole from
/// the wire (protocol.ts fences the bytes), so the slice needs no more.
function sliceRun(tabs: readonly Tab[], runStart: number, runCount: number): readonly Tab[] {
  const total = tabs.length;
  if (!(runStart >= 0 && runStart <= 255) || !(runCount >= 0 && runCount <= 255)) return tabs;
  const start = Math.trunc(runStart);
  const count = Math.trunc(runCount);
  if (start + count > total) return tabs;
  return tabs.slice(start, start + count);
}

export function initialModel(): [Model, Cmd<Msg>] {
  return [
    {
      tabs: [{ id: 1, index: 0, slot: 0, title: asciiBytes("Terminal 1"), cwd: new Uint8Array(0), selected: true, attention: false, agents: NO_AGENT_ROWS, target: NO_BYTES }],
      visibleTabs: [{ id: 1, index: 0, slot: 0, title: asciiBytes("Terminal 1"), cwd: new Uint8Array(0), selected: true, attention: false, agents: NO_AGENT_ROWS, target: NO_BYTES }],
      tabWidth: 168,
      hasOverflow: false,
      overflowLabel: new Uint8Array(0),
      selectedTab: 0,
      tabPlacement: "top",
      railRows: NO_RAIL_ROWS,
      activeWindow: 0,
      paletteOpen: false,
      mainPaletteOpen: false,
      paletteQuery: new Uint8Array(0),
      paletteAnchor: 0,
      paletteFocus: 0,
      paletteRows: NO_ROWS,
      paletteCursor: 0,
      paletteOffset: 0,
      paletteTotal: 0,
      palettePrevious: false,
      paletteNext: false,
      paletteLoading: false,
      paletteNotice: NO_BYTES,
      canReconnect: false,
      connectionStatus: asciiBytes("Starting Cockpit..."),
      window1Status: asciiBytes("Starting Cockpit..."),
      window2Status: asciiBytes("Starting Cockpit..."),
      window3Status: asciiBytes("Starting Cockpit..."),
      window4Status: asciiBytes("Starting Cockpit..."),
      settingsOpen: false,
      mainSettingsOpen: false,
      themes: NO_THEMES,
      settingsCursor: 0,
      configExists: false,
      configNotice: new Uint8Array(0),
      window1Open: false,
      window1Tabs: NO_TABS,
      window1TabWidth: 168,
      window1HasOverflow: false,
      window1OverflowLabel: new Uint8Array(0),
      window1PaletteOpen: false,
      window1SettingsOpen: false,
      window2Open: false,
      window2Tabs: NO_TABS,
      window2TabWidth: 168,
      window2HasOverflow: false,
      window2OverflowLabel: new Uint8Array(0),
      window2PaletteOpen: false,
      window2SettingsOpen: false,
      window3Open: false,
      window3Tabs: NO_TABS,
      window3TabWidth: 168,
      window3HasOverflow: false,
      window3OverflowLabel: new Uint8Array(0),
      window3PaletteOpen: false,
      window3SettingsOpen: false,
      window4Open: false,
      window4Tabs: NO_TABS,
      window4TabWidth: 168,
      window4HasOverflow: false,
      window4OverflowLabel: new Uint8Array(0),
      window4PaletteOpen: false,
      window4SettingsOpen: false,
      engineConnected: false,
      engineSequence: ZERO_U64,
      tabCommands: initialTabCommands(),
      commandResults: { ...initialCommandResults(), loading: true },
      commandNotice: NO_BYTES,
      engineRevision: ZERO_U64,
      status: asciiBytes("Starting Cockpit..."),
    },
    Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.channelOpen(ENGINE_CHANNEL_KEY, { event: "engine_event" }),
      Cmd.request("cockpit.snapshot", new Uint8Array(0), {
        key: "cockpit-snapshot",
        ok: "snapshot_loaded",
        err: "snapshot_failed",
      }),
      Cmd.request("cockpit.command-results", new Uint8Array([1, 0]), {
        key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
      }),
    ]),
  ];
}

function selectTab(tabs: readonly Tab[], selected: number): readonly Tab[] {
  return tabs.map((tab) => ({ ...tab, selected: tab.index === selected }));
}

function speculateTabTarget(model: Model, target: Uint8Array): Model {
  for (const tab of model.tabs) {
    if (!sameBytes(tab.target, target)) continue;
    const visibleTabs = selectTab(model.visibleTabs, tab.index);
    return { ...model, tabs: selectTab(model.tabs, tab.index), visibleTabs, railRows: railRows(visibleTabs), selectedTab: tab.index };
  }
  return model;
}

function tabCommandNotice(outcome: number): Uint8Array {
  if (outcome === 3) return asciiBytes("Command refused. Refresh the target and try again.");
  if (outcome === 4) return asciiBytes("Command queue full. New command not sent.");
  if (outcome === 5) return asciiBytes("Command outcome unknown; queued commands canceled.");
  if (outcome === 6) return asciiBytes("Command IDs exhausted. Restart Cockpit.");
  if (outcome === 7) return asciiBytes("Command accepted; outcome pending.");
  return NO_BYTES;
}

function tabCommandModel(model: Model, decision: TabCommandDecision): Model {
  const notice = model.commandResults.notice;
  return { ...model, tabCommands: decision.state, commandNotice: notice.length > 0 ? notice : tabCommandNotice(decision.state.outcome) };
}

function freshCommandModel(model: Model, decision: TabCommandDecision): Model {
  return tabCommandModel({ ...model, commandResults: { ...model.commandResults, notice: NO_BYTES } }, decision);
}

interface TabCommandTransition {
  readonly model: Model;
  readonly request: Uint8Array;
}

function tabCommandTransition(model: Model, msg: Msg): TabCommandTransition | null {
  if (msg.kind === "select_target") {
    const decision = enqueueTabCommand(model.tabCommands, msg.target);
    const next = freshCommandModel(model, decision);
    return { model: decision.state.outcome === 1 ? speculateTabTarget(next, msg.target) : next, request: decision.request };
  }
  if (msg.kind === "tab_command_completed") {
    const decision = receiveTabReceipt(model.tabCommands, msg.body);
    return { model: tabCommandModel(model, decision), request: decision.request };
  }
  if (msg.kind === "tab_command_failed") {
    return { model: tabCommandModel(model, unknownTabCommand(model.tabCommands)), request: NO_BYTES };
  }
  const operation = operationIntent(model, msg);
  if (operation.length === 0) return null;
  const decision = enqueueOperationCommand(model.tabCommands, operation);
  return { model: freshCommandModel(model, decision), request: decision.request };
}

function operationIntent(model: Model, msg: Msg): Uint8Array {
  switch (msg.kind) {
    case "new_terminal": return intent(2, model.engineRevision, 0, 255);
    case "new_window": return intent(8, model.engineRevision, 0, 255);
    case "close_selected_tab": return intent(3, model.engineRevision, model.selectedTab, 0);
    case "native_command":
      if (durableNativeCommand(msg.command)) return intent(11, model.engineRevision, msg.command, 255);
      return NO_BYTES;
    default: return NO_BYTES;
  }
}

function durableNativeCommand(command: number): boolean {
  return command === 3 || command === 4 || command === 5 || command === 8 || command === 9;
}

function outcomeNotice(model: Model, decision: ResultDecision): Uint8Array {
  if (decision.state.notice.length > 0) return decision.state.notice;
  if (decision.state.deliveryNotice.length > 0) {
    return model.commandNotice.length > 0 ? model.commandNotice : decision.state.deliveryNotice;
  }
  const recent = decision.state.recent;
  if (recent.length === 0) return tabCommandNotice(model.tabCommands.outcome);
  const last = recent[recent.length - 1];
  if (last.source !== 3 && sameU64(last.id, model.tabCommands.lastId)) return NO_BYTES;
  return tabCommandNotice(model.tabCommands.outcome);
}

function resultTransition(model: Model, msg: Msg): TabCommandTransition | null {
  if (msg.kind === "command_result_failed") {
    const decision = failedCommandResults(model.commandResults);
    return { model: { ...model, commandResults: decision.state, commandNotice: outcomeNotice(model, decision) }, request: decision.request };
  }
  if (msg.kind !== "command_result_loaded") return null;
  const decision = receiveCommandResult(model.commandResults, msg.body);
  return { model: { ...model, commandResults: decision.state, commandNotice: outcomeNotice(model, decision) }, request: decision.request };
}

function legacySlotIntent(revision: WireU64, slot: number): Uint8Array {
  if (!(slot >= 0 && slot <= 159)) return NO_BYTES;
  let window = 0;
  let index = Math.trunc(slot);
  while (index >= 32) {
    index -= 32;
    window += 1;
  }
  return intent(1, revision, index, window);
}

export function update(model: Model, msg: Msg): Model | [Model, Cmd<Msg>] {
  const result = resultTransition(model, msg);
  if (result !== null) {
    if (result.request.length === 0) return result.model;
    return [result.model, Cmd.request("cockpit.command-results", result.request, {
      key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
    })];
  }
  const command = tabCommandTransition(model, msg);
  if (command !== null) {
    if (command.request.length === 0) return command.model;
    return [command.model, Cmd.request("cockpit.tab-command", command.request, {
      key: "cockpit-tab-command", ok: "tab_command_completed", err: "tab_command_failed",
    })];
  }
  switch (msg.kind) {
    case "select_tab":
      return [
        {
          ...model,
          tabs: selectTab(model.tabs, msg.index),
          visibleTabs: selectTab(model.visibleTabs, msg.index),
          selectedTab: msg.index,
        },
        Cmd.host("cockpit.intent", intent(1, model.engineRevision, msg.index, 0)),
      ];
    case "select_active_tab":
      return [model, Cmd.host("cockpit.intent", intent(1, model.engineRevision, msg.index, 255))];
    case "select_slot": {
      const payload = legacySlotIntent(model.engineRevision, msg.slot);
      if (payload.length === 0) return model;
      return [model, Cmd.host("cockpit.intent", payload)];
    }
    case "reconnect":
      return [model, Cmd.host("cockpit.intent", intent(12, model.engineRevision, 0, 255))];
    case "window_closed": {
      const window = msg.window;
      if (!(window >= 1 && window <= 4)) return model;
      return [model, Cmd.host("cockpit.intent", intent(9, model.engineRevision, 0, Math.trunc(window)))];
    }
    case "toggle_tab_placement": {
      const placement: TabPlacement = model.tabPlacement === "top" ? "side" : "top";
      return [
        { ...model, tabPlacement: placement },
        Cmd.host("cockpit.intent", intent(4, model.engineRevision, placement === "side" ? 1 : 0, 0)),
      ];
    }
    case "palette_open":
    case "palette_edit":
    case "palette_move":
    case "palette_previous":
    case "palette_next":
    case "palette_retry": {
      const next = changeNavigation(model, msg);
      if (next === model || !next.paletteLoading) return next;
      return [next, Cmd.batch([
        Cmd.host("cockpit.committed", NO_BYTES),
        Cmd.request("cockpit.navigation", navigationRequest(next.engineRevision, next.paletteOffset, next.paletteQuery), {
          key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
        }),
      ])];
    }
    case "palette_close":
      return [closePalette(model), Cmd.host("cockpit.committed", NO_BYTES)];
    case "palette_submit":
    case "palette_pick": {
      const target = navigationTarget(model, msg);
      if (target.length === 0) return model;
      const decision = enqueueCatalogCommand(model.tabCommands, target);
      const next = freshCommandModel(model, decision);
      if (decision.state.outcome !== 1) return next;
      if (decision.request.length === 0) return [closePalette(next), Cmd.host("cockpit.committed", NO_BYTES)];
      return [closePalette(next), Cmd.batch([
        Cmd.host("cockpit.committed", NO_BYTES),
        Cmd.request("cockpit.tab-command", decision.request, {
          key: "cockpit-tab-command", ok: "tab_command_completed", err: "tab_command_failed",
        }),
      ])];
    }
    case "navigation_loaded":
      return loadedNavigation(model, msg.body);
    case "navigation_failed":
      if (model.paletteOffset > 0) return [requestNavigation(model, 0), Cmd.request("cockpit.navigation", navigationRequest(model.engineRevision, 0, model.paletteQuery), {
        key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
      })];
      return { ...model, paletteLoading: false, paletteNotice: asciiBytes("Workspace unavailable. Retry to refresh.") };
    case "settings_open": {
      if (model.settingsOpen) return model;
      // The probe is the engine's, once per opening, exactly as the shipping
      // app asks the disk once when the panel opens and never per frame.
      return [
        scopeOverlays({ ...model, settingsOpen: true, paletteOpen: false }),
        Cmd.batch([
          Cmd.host("cockpit.committed", NO_BYTES),
          Cmd.host("cockpit.intent", intent(7, model.engineRevision, 0, 0)),
        ]),
      ];
    }
    case "settings_close":
      return [scopeOverlays({ ...model, settingsOpen: false }), Cmd.host("cockpit.committed", NO_BYTES)];
    case "settings_move": {
      if (!model.settingsOpen || model.themes.length === 0) return model;
      const step = msg.delta >= 0 ? 1 : -1;
      const last = model.themes.length - 1;
      const raw = model.settingsCursor + step < 0 ? 0 : model.settingsCursor + step > last ? last : model.settingsCursor + step;
      const cursor = raw >= 0 && raw <= 32 ? Math.trunc(raw) : 0;
      return { ...model, settingsCursor: cursor, themes: highlightThemes(model.themes, cursor) };
    }
    case "settings_pick": {
      if (!model.settingsOpen) return model;
      const index = msg.index;
      if (!(index >= 0 && index < model.themes.length && index <= 32)) return model;
      const cursor = Math.trunc(index);
      return { ...model, settingsCursor: cursor, themes: highlightThemes(model.themes, cursor) };
    }
    case "settings_commit": {
      if (!model.settingsOpen) return model;
      return [
        scopeOverlays({ ...model, settingsOpen: false }),
        Cmd.batch([
          Cmd.host("cockpit.committed", NO_BYTES),
          Cmd.host("cockpit.intent", intent(5, model.engineRevision, model.settingsCursor, 0)),
        ]),
      ];
    }
    case "settings_reveal":
      if (!model.settingsOpen || !model.configExists) return model;
      return [model, Cmd.host("cockpit.intent", intent(6, model.engineRevision, 0, 0))];
    case "native_command":
      return [model, Cmd.host("cockpit.intent", intent(11, model.engineRevision, msg.command, 255))];
    case "engine_wake":
      return { ...model };
    case "snapshot_loaded": {
      const projected = snapshot(msg.body);
      if (projected === null) {
        return engineUnavailable(model, asciiBytes("BAD SNAPSHOT"));
      }
      // Bits 0..4 are the engine model's own limit and write refusals; bit 7
      // is the seam's: the last intent named a revision the engine had left.
      const refusedMask = projected.flags & 159;
      const rawSelected = projected.selectedTab;
      if (!(rawSelected >= 0 && rawSelected <= 255)) {
        return engineUnavailable(model, asciiBytes("BAD SNAPSHOT"));
      }
      const selectedTab = Math.trunc(rawSelected);
      const rawWidth = projected.tabWidth;
      if (!(rawWidth >= 0 && rawWidth <= 65535)) {
        return engineUnavailable(model, asciiBytes("BAD SNAPSHOT"));
      }
      const tabWidth = Math.trunc(rawWidth);
      const hidden = projected.tabs.length - projected.runCount;
      const active = projected.activeTheme;
      const cursor = model.settingsOpen ? model.settingsCursor : active >= 0 && active <= 32 && active < projected.themes.length ? Math.trunc(active) : 0;
      const refused = (projected.flags & 8) !== 0;
      const mainTabs = stampSlots(projected.tabs, 0, projected.agents);
      const mainVisible = sliceRun(mainTabs, projected.runStart, projected.runCount);
      const w1 = windowState(1, findSection(projected.secondary, 1), projected.agents);
      const w2 = windowState(2, findSection(projected.secondary, 2), projected.agents);
      const w3 = windowState(3, findSection(projected.secondary, 3), projected.agents);
      const w4 = windowState(4, findSection(projected.secondary, 4), projected.agents);
      // The width crosses a record into an integer slot; the proof is
      // restated at the boundary, once per slot.
      const width1 = w1.tabWidth >= 0 && w1.tabWidth <= 65535 ? Math.trunc(w1.tabWidth) : 168;
      const width2 = w2.tabWidth >= 0 && w2.tabWidth <= 65535 ? Math.trunc(w2.tabWidth) : 168;
      const width3 = w3.tabWidth >= 0 && w3.tabWidth <= 65535 ? Math.trunc(w3.tabWidth) : 168;
      const width4 = w4.tabWidth >= 0 && w4.tabWidth <= 65535 ? Math.trunc(w4.tabWidth) : 168;
      const synced: Model = {
        ...model,
        activeWindow: projected.activeWindow >= 0 && projected.activeWindow <= 4 ? Math.trunc(projected.activeWindow) : 0,
        window1Open: w1.open,
        window1Tabs: w1.visibleTabs,
        window1TabWidth: width1,
        window1HasOverflow: w1.hasOverflow,
        window1OverflowLabel: w1.overflowLabel,
        window2Open: w2.open,
        window2Tabs: w2.visibleTabs,
        window2TabWidth: width2,
        window2HasOverflow: w2.hasOverflow,
        window2OverflowLabel: w2.overflowLabel,
        window3Open: w3.open,
        window3Tabs: w3.visibleTabs,
        window3TabWidth: width3,
        window3HasOverflow: w3.hasOverflow,
        window3OverflowLabel: w3.overflowLabel,
        window4Open: w4.open,
        window4Tabs: w4.visibleTabs,
        window4TabWidth: width4,
        window4HasOverflow: w4.hasOverflow,
        window4OverflowLabel: w4.overflowLabel,
        themes: themeRows(projected.themes, active, cursor),
        settingsCursor: cursor,
        configExists: projected.configExists,
        configNotice: configNotice(projected.configEnabled, projected.configProbed, projected.configExists, projected.configWritable, refused, projected.configPath),
        tabs: mainTabs,
        visibleTabs: mainVisible,
        railRows: railRows(mainVisible),
        tabWidth,
        hasOverflow: hidden > 0,
        overflowLabel: hidden > 0 ? overflowLabel(hidden) : new Uint8Array(0),
        selectedTab,
        tabPlacement: projected.tabPlacement === 1 ? "side" : "top",
        engineConnected: true,
        engineSequence: projected.sequence,
        engineRevision: projected.revision,
        canReconnect: projected.connection === 3,
        connectionStatus: windowStatus(projected.connection, projected.terminalStates[0], refusedMask !== 0),
        window1Status: windowStatus(projected.connection, projected.terminalStates[1], refusedMask !== 0),
        window2Status: windowStatus(projected.connection, projected.terminalStates[2], refusedMask !== 0),
        window3Status: windowStatus(projected.connection, projected.terminalStates[3], refusedMask !== 0),
        window4Status: windowStatus(projected.connection, projected.terminalStates[4], refusedMask !== 0),
        status: refusedMask === 0 ? asciiBytes("READY") : asciiBytes("ACTION REFUSED"),
      };
      const scoped = scopeOverlays(synced);
      if (!model.paletteOpen) return scoped;
      return [refreshNavigation(scoped), Cmd.request("cockpit.navigation", navigationRequest(scoped.engineRevision, model.paletteOffset, model.paletteQuery), {
        key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
      })];
    }
    case "snapshot_failed":
      return engineUnavailable(model, asciiBytes("ENGINE UNAVAILABLE"));
    case "engine_event": {
      if (msg.state !== "data") {
        return engineUnavailable(model, msg.state === "rejected" ? asciiBytes("ENGINE REFUSED") : asciiBytes("ENGINE CLOSED"));
      }
      const event = invalidation(msg.bytes);
      if (event === null) return { ...model, status: asciiBytes("ENGINE PROTOCOL ERROR") };
      const read = requestCommandResults(model.commandResults);
      const next = {
        ...model,
        commandResults: read.state,
        engineSequence: event.sequence,
        engineConnected: false,
        status: asciiBytes("SYNCING"),
        paletteRows: NO_ROWS,
        paletteLoading: model.paletteOpen,
        palettePrevious: false,
        paletteNext: false,
      };
      if (read.request.length > 0) return [next, Cmd.batch([
        Cmd.request("cockpit.snapshot", NO_BYTES, {
          key: "cockpit-snapshot", ok: "snapshot_loaded", err: "snapshot_failed",
        }),
        Cmd.request("cockpit.command-results", read.request, {
          key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
        }),
      ])];
      return [
        next,
        Cmd.request("cockpit.snapshot", new Uint8Array(0), {
          key: "cockpit-snapshot",
          ok: "snapshot_loaded",
          err: "snapshot_failed",
        }),
      ];
    }
    default:
      return model;
  }
}
