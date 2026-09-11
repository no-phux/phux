import { Cmd, asciiBytes, utf8Bytes, windowDescriptor } from "@native-sdk/core";
import { type WindowDescriptor } from "@native-sdk/core/events";
import { applyTextInputEvent, type TextEditState, type TextInputEvent } from "@native-sdk/core/text";
import { type TabCommandState, type TabCommandDecision, initialTabCommands, enqueueTabCommand, enqueueCatalogCommand, enqueueOperationCommand, receiveTabReceipt, unknownTabCommand } from "./tab-commands.ts";
import { type CommandResults, type ResultDecision, initialCommandResults, requestCommandResults, receiveCommandResult, failedCommandResults } from "./command-results.ts";
import { type Appearance, initialAppearance, appearanceRequest, appearanceResponse } from "./appearance.ts";
import {
  REMOTE_KIND_STATUS,
  REMOTE_KIND_CONNECT,
  REMOTE_KIND_LOCAL,
  REMOTE_KIND_DISCONNECT,
  REMOTE_PHASE_LOCAL,
  REMOTE_PHASE_CONNECTED,
  REMOTE_PHASE_FAILED,
  remoteRequest,
  remoteReply,
  remoteStatusLine,
} from "./remote-hosts.ts";
import {
  DIR_KIND_OPEN,
  DIR_KIND_PAGE,
  DIR_KIND_DESCEND,
  DIR_KIND_PARENT,
  DIR_KIND_HERE,
  DIR_STATUS_PENDING,
  DIR_STATUS_LISTED,
  DIR_HERE,
  DIR_UP,
  NO_DIRECTORY_REQUEST,
  type DirectoryPage,
  directoryRequest,
  directoryPage,
  directoryRowLabel,
  directoryNotice,
  directoryTitle,
  DIR_OTHER_COORDINATOR_NOTICE,
} from "./directory.ts";
import {
  ENGINE_CHANNEL_KEY,
  type WireU64,
  type SnapshotTab,
  type SecondaryWindow,
  invalidation,
  intent,
  sameU64,
  snapshot,
  navigationScopedRequest,
  type NavigationRow,
  type NavigationPage,
  navigationPage,
  navigationHostFilter,
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
  readonly detail: Uint8Array;
  readonly kind: number;
  readonly host: Uint8Array;
  readonly selectable: boolean;
  readonly disabled: boolean;
}

export interface ThemeRow {
  readonly index: number;
  readonly label: Uint8Array;
  readonly active: boolean;
  readonly highlighted: boolean;
}

export interface SettingsChoice {
  readonly index: number;
  readonly label: Uint8Array;
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
  readonly workspaceLabel: Uint8Array;
  readonly window1RailRows: readonly RailRow[];
  readonly window2RailRows: readonly RailRow[];
  readonly window3RailRows: readonly RailRow[];
  readonly window4RailRows: readonly RailRow[];
  /// Native projection slot for the platform window that owns modal chrome.
  /// Platform ids never cross the seam; the snapshot carries only 0..4.
  readonly activeWindow: number;
  readonly paletteOpen: boolean;
  readonly mainPaletteOpen: boolean;
  readonly paletteQuery: Uint8Array;
  readonly paletteScope: number;
  readonly paletteHost: Uint8Array;
  readonly paletteHostLabel: Uint8Array;
  readonly navigationScopes: readonly SettingsChoice[];
  readonly coordinatorEndpoint: Uint8Array;
  readonly connectionDetail: Uint8Array;
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
  /// Connect to Host (remote-hosts.ts): an app-wide modal like the switcher,
  /// presented in whichever window invoked it (the snapshot's active window).
  /// `hostQuery` survives a failure so a retry is one keystroke;
  /// `hostAwaiting` holds the panel open until the engine reports the host
  /// connected or failed.
  readonly hostOpen: boolean;
  readonly mainHostOpen: boolean;
  readonly window1HostOpen: boolean;
  readonly window2HostOpen: boolean;
  readonly window3HostOpen: boolean;
  readonly window4HostOpen: boolean;
  /// Go to Directory (directory.ts): an app-wide modal in the invoking window
  /// like Connect to Host. `dirRequest` is the listing the rows came from,
  /// as four opaque bytes; `dirStarting` accepts the next listing's new ID;
  /// `dirAwaiting` polls on each invalidation until that listing settles;
  /// `dirClosing` closes the picker once Open Here is accepted.
  readonly dirOpen: boolean;
  readonly mainDirOpen: boolean;
  readonly window1DirOpen: boolean;
  readonly window2DirOpen: boolean;
  readonly window3DirOpen: boolean;
  readonly window4DirOpen: boolean;
  readonly dirQuery: Uint8Array;
  readonly dirAnchor: number;
  readonly dirFocus: number;
  readonly dirRequest: Uint8Array;
  readonly dirStarting: boolean;
  readonly dirAwaiting: boolean;
  readonly dirClosing: boolean;
  readonly dirBusy: boolean;
  readonly dirRows: readonly DirRow[];
  readonly dirCursor: number;
  readonly dirOffset: number;
  readonly dirPrevious: boolean;
  readonly dirNext: boolean;
  readonly dirPath: Uint8Array;
  readonly dirNotice: Uint8Array;
  /// The picker's heading: names the satellite a listing comes from.
  readonly dirTitle: Uint8Array;
  /// Open Here can open a tab in this listing (the active coordinator's).
  readonly dirOpenHere: boolean;
  readonly hostQuery: Uint8Array;
  readonly hostAnchor: number;
  readonly hostFocus: number;
  readonly hostNotice: Uint8Array;
  readonly hostBusy: boolean;
  readonly hostAwaiting: boolean;
  /// Last engine answer: phase 0 local .. 4 reconnecting, and the host name.
  readonly hostPhase: number;
  readonly hostName: Uint8Array;
  readonly remoteLine: Uint8Array;
  /// Snapshot connection byte last seen; a change asks for remote status.
  readonly lastConnection: number;
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
  readonly appearance: Appearance;
  readonly appearanceBusy: boolean;
  readonly settingsSection: number;
  readonly settingsSections: readonly SettingsChoice[];
  readonly cursorChoices: readonly SettingsChoice[];
  readonly placementChoices: readonly SettingsChoice[];
  readonly fontDecrease: number;
  readonly fontIncrease: number;
  readonly navigationAfterSettings: boolean;
  /// A rollback (Cancel, Escape, or leaving for the switcher) is in flight;
  /// if it fails, Settings still closes rather than trapping the keyboard.
  readonly appearanceClosing: boolean;
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
  | { readonly kind: "palette_scope"; readonly scope: number }
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
  | { readonly kind: "host_open" }
  | { readonly kind: "host_close" }
  | { readonly kind: "host_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "host_submit" }
  | { readonly kind: "host_local" }
  | { readonly kind: "host_disconnect" }
  | { readonly kind: "dir_open" }
  | { readonly kind: "dir_close" }
  | { readonly kind: "dir_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "dir_submit" }
  | { readonly kind: "dir_pick"; readonly index: number }
  | { readonly kind: "dir_here" }
  | { readonly kind: "dir_previous" }
  | { readonly kind: "dir_next" }
  | { readonly kind: "directory_loaded"; readonly body: Uint8Array }
  | { readonly kind: "directory_failed"; readonly error: Uint8Array }
  | { readonly kind: "remote_loaded"; readonly body: Uint8Array }
  | { readonly kind: "remote_failed"; readonly error: Uint8Array }
  | { readonly kind: "settings_open" }
  | { readonly kind: "settings_close" }
  | { readonly kind: "settings_move"; readonly delta: number }
  | { readonly kind: "settings_pick"; readonly index: number }
  | { readonly kind: "settings_commit" }
  | { readonly kind: "settings_reveal" }
  | { readonly kind: "settings_section"; readonly section: number }
  | { readonly kind: "settings_font"; readonly direction: number }
  | { readonly kind: "settings_cursor"; readonly index: number }
  | { readonly kind: "settings_placement"; readonly index: number }
  | { readonly kind: "appearance_loaded"; readonly body: Uint8Array }
  | { readonly kind: "appearance_failed"; readonly error: Uint8Array }
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
  "appearance_loaded",
  "appearance_failed",
  "navigationAfterSettings",
  "appearanceClosing",
  "engineConnected",
  "engineSequence",
  "engineRevision",
  "status",
  "engine_event",
  "engine_wake",
  "snapshot_loaded",
  "snapshot_failed",
  "native_command",
  "hostOpen",
  "hostAnchor",
  "hostFocus",
  "hostBusy",
  "hostAwaiting",
  "hostPhase",
  "hostName",
  "remoteLine",
  "lastConnection",
  "remote_loaded",
  "remote_failed",
  "dirOpen",
  "dirAnchor",
  "dirFocus",
  "dirRequest",
  "dirStarting",
  "dirAwaiting",
  "dirClosing",
  "dirBusy",
  "dirCursor",
  "dirOffset",
  "dir_open",
  "directory_loaded",
  "directory_failed",
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

/// A painted pick carries its own captured bytes; keyboard submission uses the
/// highlighted current row. A current row the engine marked unselectable never
/// submits. A held pick absent from the page stays native-validated.
function navigationTarget(model: Model, msg: Msg): Uint8Array {
  if (!model.paletteOpen) return NO_BYTES;
  if (msg.kind === "palette_pick") return currentRowRefuses(model, msg.target) ? NO_BYTES : msg.target;
  if (model.paletteLoading || model.paletteRows.length === 0) return NO_BYTES;
  const row = model.paletteRows[model.paletteCursor];
  return row.selectable ? row.target : NO_BYTES;
}

function currentRowRefuses(model: Model, target: Uint8Array): boolean {
  for (const row of model.paletteRows) {
    if (sameBytes(row.target, target)) return !row.selectable;
  }
  return false;
}

function loadedNavigation(model: Model, body: Uint8Array): Model {
  if (!model.paletteOpen || !model.engineConnected) return model;
  const page = navigationPage(body);
  if (page === null) return { ...model, paletteLoading: false, paletteNotice: asciiBytes("Workspace unavailable. Retry to refresh.") };
  if (!currentNavigationPage(model, page)) return model;
  const total = page.total >= 0 && page.total <= 65535 ? Math.trunc(page.total) : 0;
  const loaded: Model = { ...model, paletteRows: switcherRows(page.rows), paletteTotal: total, paletteLoading: false,
    palettePrevious: page.offset > 0, paletteNext: page.offset + page.rows.length < total,
    paletteNotice: navigationNotice(model.paletteScope, total) };
  return highlightNavigation(loaded, Math.min(model.paletteCursor, page.rows.length - 1));
}

function currentNavigationPage(model: Model, page: NavigationPage): boolean {
  return sameU64(page.revision, model.engineRevision) && page.offset === model.paletteOffset &&
    sameBytes(page.query, model.paletteQuery) && page.scope === model.paletteScope && sameBytes(page.host, model.paletteHost);
}

function switcherRows(rows: readonly NavigationRow[]): readonly SwitcherRow[] {
  const result: SwitcherRow[] = [];
  for (const row of rows) {
    const index = row.index >= 0 && row.index <= 65535 ? Math.trunc(row.index) : 0;
    const kind = row.kind >= 0 && row.kind <= 3 ? Math.trunc(row.kind) : 0;
    result.push({ id: index, index, kind, label: row.label, detail: row.detail, host: row.host,
      highlighted: row.highlighted, selectable: row.selectable, disabled: !row.selectable, target: row.target });
  }
  return result;
}

function navigationNotice(scope: number, total: number): Uint8Array {
  if (scope === 2) return total === 0 ? asciiBytes("No known terminal hosts on this connection") : asciiBytes("Hosts represented by known terminals");
  if (scope === 1) return total === 0 ? asciiBytes("No matching Phux sessions") : asciiBytes("Select a session to open its workspace");
  return total === 0 ? asciiBytes("No matching terminals or sessions") : asciiBytes("Enter to open  /  Escape to return");
}

function scopedNavigationRequest(model: Model): Uint8Array {
  return navigationScopedRequest(model.engineRevision, model.paletteOffset, model.paletteQuery, model.paletteScope, model.paletteHost);
}

function chooseNavigationScope(model: Model, scope: number): Model {
  if (!model.paletteOpen || !(scope >= 0 && scope <= 2)) return model;
  return requestNavigation({ ...model, paletteScope: Math.trunc(scope), paletteHost: NO_BYTES, paletteHostLabel: NO_BYTES,
    paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0 }, 0);
}

/// A known-host token only narrows the view to its exact raw host; it is never
/// enqueued as catalog authority, even when held across a replacement page.
function hostNavigation(model: Model, host: Uint8Array): Model {
  const exact = host.slice();
  const label = exact.length === 0 ? asciiBytes("Coordinator") : exact;
  return requestNavigation({ ...model, paletteScope: 3, paletteHost: exact, paletteHostLabel: label,
    paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0 }, 0);
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
      return requestNavigation(scopeOverlays({ ...model, paletteOpen: true, settingsOpen: false, hostOpen: false, hostAwaiting: false, paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0,
        paletteScope: 0, paletteHost: NO_BYTES, paletteHostLabel: NO_BYTES }), 0);
    case "palette_scope": return chooseNavigationScope(model, msg.scope);
    case "palette_edit": return editNavigation(model, msg.edit);
    case "palette_move": return moveNavigation(model, msg.delta);
    default: return browseNavigation(model, msg);
  }
}

/// One drawn row of Go to Directory: a listing index (or a synthetic role,
/// directory.ts DIR_HERE / DIR_UP) and what it says. A pick echoes only the
/// index; the engine composes the path from the listing it names.
export interface DirRow {
  readonly id: number;
  readonly index: number;
  readonly label: Uint8Array;
  readonly highlighted: boolean;
}

const NO_DIR_ROWS: readonly DirRow[] = [];

/// What one Go to Directory message leaves: the model, the request to send
/// (empty for none), and whether the modal slot changed hands. `update`
/// builds the effects inline, as the compiled subset requires.
interface DirectoryDecision {
  readonly model: Model;
  readonly request: Uint8Array;
  readonly committed: boolean;
}

function directoryDecision(model: Model, request: Uint8Array, committed: boolean): DirectoryDecision {
  return { model, request, committed };
}

function unchangedDirectory(model: Model): DirectoryDecision {
  return directoryDecision(model, NO_BYTES, false);
}

function directoryState(model: Model): TextEditState {
  return {
    text: model.dirQuery,
    selection: { anchor: model.dirAnchor, focus: model.dirFocus },
    composition: null,
  };
}

function pageDirectory(model: Model, offset: number): DirectoryDecision {
  const at = offset >= 0 && offset <= 65535 ? Math.trunc(offset) : 0;
  const next = { ...model, dirOffset: at };
  return directoryDecision(next, directoryRequest(DIR_KIND_PAGE, next.dirRequest, at, 0, next.dirQuery), false);
}

/// Go to Directory takes the one modal slot: the switcher, Connect to Host
/// and Settings give way to it as they do to each other.
function openDirectory(model: Model): DirectoryDecision {
  if (model.dirOpen) return unchangedDirectory(model);
  const base = model.paletteOpen ? closePalette(model) : model;
  const next = scopeOverlays({ ...base, dirOpen: true, hostOpen: false, hostAwaiting: false, settingsOpen: false,
    dirQuery: NO_BYTES, dirAnchor: 0, dirFocus: 0, dirRequest: NO_DIRECTORY_REQUEST, dirStarting: true,
    dirAwaiting: false, dirClosing: false, dirBusy: true, dirRows: NO_DIR_ROWS, dirCursor: 0, dirOffset: 0,
    dirPrevious: false, dirNext: false, dirPath: NO_BYTES, dirNotice: asciiBytes("Listing..."),
    dirTitle: asciiBytes("Go to Directory"), dirOpenHere: true });
  return directoryDecision(next, directoryRequest(DIR_KIND_OPEN, NO_DIRECTORY_REQUEST, 0, 0, NO_BYTES), true);
}

function closeDirectory(model: Model): DirectoryDecision {
  return directoryDecision(scopeOverlays({ ...model, dirOpen: false, dirQuery: NO_BYTES, dirRows: NO_DIR_ROWS,
    dirBusy: false, dirAwaiting: false, dirStarting: false, dirClosing: false }), NO_BYTES, true);
}

/// Another modal opening while the picker is up takes its slot.
function displaceDirectory(model: Model, msg: Msg): Model {
  if (!model.dirOpen) return model;
  if (msg.kind !== "palette_open" && msg.kind !== "host_open" && msg.kind !== "settings_open") return model;
  return closeDirectory(model).model;
}

/// The connection under an open picker moved, so its rows named a listing on
/// the old connection. Withdraw them; once connected again, list afresh.
function relistDirectory(model: Model, connected: boolean): Model {
  return { ...model, dirRows: NO_DIR_ROWS, dirPrevious: false, dirNext: false, dirAwaiting: false, dirClosing: false,
    dirStarting: connected, dirBusy: connected, dirRequest: NO_DIRECTORY_REQUEST, dirQuery: NO_BYTES, dirAnchor: 0,
    dirFocus: 0, dirOffset: 0, dirCursor: 0,
    dirNotice: connected ? asciiBytes("Reconnected. Listing again...") : asciiBytes("Waiting for the connection...") };
}

function directoryRows(page: DirectoryPage): readonly DirRow[] {
  const rows: DirRow[] = [];
  for (const row of page.rows) {
    const index = row.index >= 0 && row.index <= 65535 ? Math.trunc(row.index) : 0;
    rows.push({ id: index, index, label: directoryRowLabel(row), highlighted: false });
  }
  return rows.length === 0 ? NO_DIR_ROWS : rows;
}

function highlightDirectory(model: Model, next: number): Model {
  if (next < 0 || next >= model.dirRows.length) return model;
  const cursor = next >= 0 && next <= 3 ? Math.trunc(next) : 0;
  const rows: DirRow[] = [];
  for (let i = 0; i < model.dirRows.length; i += 1) {
    const row = model.dirRows[i];
    rows.push({ ...row, highlighted: i === cursor });
  }
  return { ...model, dirCursor: cursor, dirRows: rows };
}

function showDirectory(model: Model, page: DirectoryPage): Model {
  const total = page.total >= 0 && page.total <= 65535 ? Math.trunc(page.total) : 0;
  const rows = directoryRows(page);
  const shown: Model = { ...model, dirRequest: page.request, dirStarting: false, dirBusy: false,
    dirAwaiting: page.status === DIR_STATUS_PENDING, dirRows: rows, dirCursor: 0,
    dirPath: page.path.length > 0 ? page.path : model.dirPath, dirTitle: directoryTitle(page), dirOpenHere: page.via.length === 0,
    dirPrevious: page.offset > 0, dirNext: page.offset + page.rows.length < total, dirNotice: directoryNotice(page) };
  return highlightDirectory(shown, Math.min(model.dirCursor, rows.length - 1));
}

/// A reply counts only while the picker is open, for the listing it shows
/// (or the one it has just started), and for the page it last asked for. A
/// reply that arrives after Escape, or for a directory already left, is
/// therefore dropped.
function receiveDirectory(model: Model, body: Uint8Array): DirectoryDecision {
  if (!model.dirOpen) return unchangedDirectory(model);
  const page = directoryPage(body);
  if (page === null) {
    return unchangedDirectory({ ...model, dirBusy: false, dirClosing: false,
      dirNotice: asciiBytes("Directory listing unavailable. Try again.") });
  }
  if (!model.dirStarting && !sameBytes(page.request, model.dirRequest)) return unchangedDirectory(model);
  if (model.dirClosing) return closeDirectory(model);
  if (page.offset !== model.dirOffset || !sameBytes(page.query, model.dirQuery)) {
    return unchangedDirectory({ ...model, dirRequest: page.request, dirStarting: false });
  }
  return unchangedDirectory(showDirectory(model, page));
}

/// Open Here refused keeps the picker for another try. A refused descend or
/// parent named a listing the engine has moved past, so show the current one.
function failedDirectory(model: Model): DirectoryDecision {
  if (!model.dirOpen) return unchangedDirectory(model);
  if (model.dirClosing) {
    return unchangedDirectory({ ...model, dirBusy: false, dirClosing: false,
      dirNotice: asciiBytes("Could not open a new tab there. Try again.") });
  }
  if (!model.dirStarting) {
    return unchangedDirectory({ ...model, dirBusy: false, dirNotice: asciiBytes("Directory listing unavailable. Try again.") });
  }
  return directoryDecision({ ...model, dirBusy: false, dirNotice: asciiBytes("That listing changed. Showing the current one.") },
    directoryRequest(DIR_KIND_PAGE, NO_DIRECTORY_REQUEST, 0, 0, NO_BYTES), false);
}

function moveDirectory(model: Model, delta: number): DirectoryDecision {
  if (model.dirBusy || model.dirStarting) return unchangedDirectory(model);
  const next = model.dirCursor + (delta >= 0 ? 1 : -1);
  if (next < 0 && model.dirPrevious) {
    const previous = pageDirectory(model, model.dirOffset - 4);
    return directoryDecision({ ...previous.model, dirCursor: 3 }, previous.request, false);
  }
  if (next >= model.dirRows.length && model.dirNext) {
    const following = pageDirectory(model, model.dirOffset + 4);
    return directoryDecision({ ...following.model, dirCursor: 0 }, following.request, false);
  }
  return unchangedDirectory(highlightDirectory(model, next));
}

function editDirectory(model: Model, edit: TextInputEvent): DirectoryDecision {
  const next = applyTextInputEvent(directoryState(model), edit, 64);
  if (next === null) return unchangedDirectory(model);
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 64 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 64 ? Math.trunc(next.selection.focus) : 0;
  return pageDirectory({ ...model, dirQuery: next.text, dirAnchor: anchor, dirFocus: focus, dirCursor: 0 }, 0);
}

/// Descend or go up: a new listing under a new request ID, which the next
/// reply names and the core adopts.
function startDirectoryListing(model: Model, kind: number, index: number): DirectoryDecision {
  return directoryDecision({ ...model, dirStarting: true, dirBusy: true, dirAwaiting: false, dirQuery: NO_BYTES,
    dirAnchor: 0, dirFocus: 0, dirOffset: 0, dirCursor: 0, dirRows: NO_DIR_ROWS, dirPrevious: false, dirNext: false,
    dirNotice: asciiBytes("Listing...") }, directoryRequest(kind, model.dirRequest, 0, index, NO_BYTES), false);
}

function activateDirectory(model: Model, index: number): DirectoryDecision {
  if (model.dirBusy || model.dirStarting) return unchangedDirectory(model);
  const row = index >= 0 && index <= 65535 ? Math.trunc(index) : 0;
  if (row === DIR_UP) return startDirectoryListing(model, DIR_KIND_PARENT, 0);
  // A listing through another coordinator: say so, and send nothing.
  if (row === DIR_HERE && !model.dirOpenHere) return unchangedDirectory({ ...model, dirNotice: asciiBytes(DIR_OTHER_COORDINATOR_NOTICE) });
  if (row === DIR_HERE) {
    return directoryDecision({ ...model, dirBusy: true, dirClosing: true, dirNotice: asciiBytes("Opening a new tab...") },
      directoryRequest(DIR_KIND_HERE, model.dirRequest, model.dirOffset, DIR_HERE, model.dirQuery), false);
  }
  return startDirectoryListing(model, DIR_KIND_DESCEND, row);
}

function submitDirectory(model: Model): DirectoryDecision {
  if (model.dirCursor < 0 || model.dirCursor >= model.dirRows.length) return unchangedDirectory(model);
  return activateDirectory(model, model.dirRows[model.dirCursor].index);
}

function browseDirectory(model: Model, forward: boolean): DirectoryDecision {
  if (model.dirBusy) return unchangedDirectory(model);
  if (forward) return model.dirNext ? pageDirectory(model, model.dirOffset + 4) : unchangedDirectory(model);
  return model.dirPrevious ? pageDirectory(model, model.dirOffset - 4) : unchangedDirectory(model);
}

/// Every message the open picker answers. Escape (palette_close) and the
/// arrows (palette_move) reach it through the shared overlay keys.
function openDirectoryTransition(model: Model, msg: Msg): DirectoryDecision | null {
  switch (msg.kind) {
    case "dir_close":
    case "palette_close": return closeDirectory(model);
    case "palette_move": return moveDirectory(model, msg.delta);
    case "dir_edit": return editDirectory(model, msg.edit);
    case "dir_submit": return submitDirectory(model);
    case "dir_pick": return activateDirectory(model, msg.index);
    case "dir_here": return activateDirectory(model, DIR_HERE);
    case "dir_previous": return browseDirectory(model, false);
    case "dir_next": return browseDirectory(model, true);
    default: return null;
  }
}

function directoryTransition(model: Model, msg: Msg): DirectoryDecision | null {
  if (msg.kind === "dir_open") return openDirectory(model);
  if (msg.kind === "directory_loaded") return receiveDirectory(model, msg.body);
  if (msg.kind === "directory_failed") return failedDirectory(model);
  if (!model.dirOpen) return null;
  return openDirectoryTransition(model, msg);
}

function hostState(model: Model): TextEditState {
  return {
    text: model.hostQuery,
    selection: { anchor: model.hostAnchor, focus: model.hostFocus },
    composition: null,
  };
}

function editHost(model: Model, edit: TextInputEvent): Model {
  if (!model.hostOpen) return model;
  const next = applyTextInputEvent(hostState(model), edit, 255);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 255 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 255 ? Math.trunc(next.selection.focus) : 0;
  return { ...model, hostQuery: next.text, hostAnchor: anchor, hostFocus: focus };
}

/// The switcher and the host panel are one modal slot: opening Connect to
/// Host from the switcher replaces it. The previous host stays in the field.
function openHost(model: Model): Model {
  const base = model.paletteOpen ? closePalette(model) : model;
  const length = base.hostQuery.length;
  const end = length >= 0 && length <= 255 ? Math.trunc(length) : 0;
  return scopeOverlays({ ...base, hostOpen: true, settingsOpen: false, hostAnchor: end, hostFocus: end,
    hostNotice: base.remoteLine.length > 0 ? base.remoteLine : asciiBytes("A host registered with phux host add or phux host enroll") });
}

/// Apply one engine answer. A failure keeps the panel and the typed host;
/// a connection the panel was waiting for closes it.
function receiveRemote(model: Model, body: Uint8Array): Model {
  const reply = remoteReply(body);
  if (reply === null) {
    return { ...model, hostBusy: false, hostAwaiting: false, hostNotice: asciiBytes("Connection status unavailable. Try again.") };
  }
  const line = remoteStatusLine(reply);
  const settled = reply.phase === REMOTE_PHASE_CONNECTED || reply.phase === REMOTE_PHASE_FAILED || reply.phase === REMOTE_PHASE_LOCAL;
  const done = model.hostAwaiting && (reply.phase === REMOTE_PHASE_CONNECTED || reply.phase === REMOTE_PHASE_LOCAL);
  return scopeOverlays({
    ...model,
    hostBusy: false,
    hostAwaiting: model.hostAwaiting && !settled,
    hostOpen: model.hostOpen && !done,
    hostPhase: reply.phase >= 0 && reply.phase <= 4 ? Math.trunc(reply.phase) : 0,
    hostName: reply.host,
    remoteLine: line,
    hostNotice: line.length > 0 ? line : asciiBytes("Using this Mac's Phux coordinator"),
    connectionStatus: line.length > 0 ? line : model.connectionStatus,
  });
}

/// While a remote host is selected the status bar names it; a failure shows
/// its reason instead of the generic offline label.
function remoteConnectionStatus(model: Model, local: Uint8Array): Uint8Array {
  if (model.hostPhase === REMOTE_PHASE_LOCAL || model.hostName.length === 0) return local;
  if (model.hostPhase === REMOTE_PHASE_FAILED) return model.remoteLine;
  return joinBytes(model.hostName, asciiBytes(" / "), local);
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
    mainHostOpen: model.hostOpen && active === 0,
    window1HostOpen: model.hostOpen && active === 1,
    window2HostOpen: model.hostOpen && active === 2,
    window3HostOpen: model.hostOpen && active === 3,
    window4HostOpen: model.hostOpen && active === 4,
    mainDirOpen: model.dirOpen && active === 0,
    window1DirOpen: model.dirOpen && active === 1,
    window2DirOpen: model.dirOpen && active === 2,
    window3DirOpen: model.dirOpen && active === 3,
    window4DirOpen: model.dirOpen && active === 4,
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
  if (name === "remote.connect") return { kind: "host_open" };
  if (name === "directory.open") return { kind: "dir_open" };
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
      workspaceLabel: asciiBytes("Workspace"),
      window1RailRows: NO_RAIL_ROWS,
      window2RailRows: NO_RAIL_ROWS,
      window3RailRows: NO_RAIL_ROWS,
      window4RailRows: NO_RAIL_ROWS,
      activeWindow: 0,
      paletteOpen: false,
      mainPaletteOpen: false,
      paletteQuery: new Uint8Array(0),
      paletteScope: 0,
      paletteHost: NO_BYTES,
      paletteHostLabel: NO_BYTES,
      navigationScopes: [{ index: 0, label: asciiBytes("All work") }, { index: 1, label: asciiBytes("Sessions") }, { index: 2, label: asciiBytes("Known hosts") }],
      coordinatorEndpoint: NO_BYTES,
      connectionDetail: NO_BYTES,
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
      hostOpen: false,
      mainHostOpen: false,
      window1HostOpen: false,
      window2HostOpen: false,
      window3HostOpen: false,
      window4HostOpen: false,
      dirOpen: false,
      mainDirOpen: false,
      window1DirOpen: false,
      window2DirOpen: false,
      window3DirOpen: false,
      window4DirOpen: false,
      dirQuery: new Uint8Array(0),
      dirAnchor: 0,
      dirFocus: 0,
      dirRequest: NO_DIRECTORY_REQUEST,
      dirStarting: false,
      dirAwaiting: false,
      dirClosing: false,
      dirBusy: false,
      dirRows: NO_DIR_ROWS,
      dirCursor: 0,
      dirOffset: 0,
      dirPrevious: false,
      dirNext: false,
      dirPath: new Uint8Array(0),
      dirNotice: new Uint8Array(0),
      dirTitle: asciiBytes("Go to Directory"),
      dirOpenHere: true,
      hostQuery: new Uint8Array(0),
      hostAnchor: 0,
      hostFocus: 0,
      hostNotice: NO_BYTES,
      hostBusy: false,
      hostAwaiting: false,
      hostPhase: 0,
      hostName: new Uint8Array(0),
      remoteLine: new Uint8Array(0),
      // No byte value: the first snapshot always asks which host is selected,
      // so a host restored at launch is named from the start.
      lastConnection: 255,
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
      appearance: initialAppearance(),
      appearanceBusy: false,
      settingsSection: 0,
      settingsSections: [
        { index: 0, label: asciiBytes("Appearance") }, { index: 1, label: asciiBytes("Workspace") }, { index: 2, label: asciiBytes("Connection") },
      ],
      cursorChoices: [
        { index: 0, label: asciiBytes("Block") }, { index: 1, label: asciiBytes("Bar") }, { index: 2, label: asciiBytes("Underline") },
      ],
      placementChoices: [{ index: 0, label: asciiBytes("Top strip") }, { index: 1, label: asciiBytes("Workspace rail") }],
      fontDecrease: 0,
      fontIncrease: 1,
      navigationAfterSettings: false,
      appearanceClosing: false,
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

interface AppearanceDecision {
  readonly model: Model;
  readonly request: Uint8Array;
  readonly opening: boolean;
  readonly closed: boolean;
  readonly navigate: boolean;
}

function appearanceDecision(model: Model): AppearanceDecision {
  return { model, request: NO_BYTES, opening: false, closed: false, navigate: false };
}

function requestAppearance(model: Model, action: number, argument: number): AppearanceDecision {
  return { ...appearanceDecision({ ...model, appearanceBusy: true }), request: appearanceRequest(action, argument) };
}

function openAppearance(model: Model): AppearanceDecision {
  if (model.settingsOpen) return appearanceDecision(model);
  const next = scopeOverlays({ ...model, settingsOpen: true, paletteOpen: false, hostOpen: false, hostAwaiting: false, settingsSection: 0,
    navigationAfterSettings: false, appearanceClosing: false, appearance: initialAppearance(), appearanceBusy: true });
  return { ...requestAppearance(next, 0, 0), opening: true };
}

function loadedAppearance(model: Model, body: Uint8Array): AppearanceDecision {
  if (!model.settingsOpen) return appearanceDecision(model);
  const appearance = appearanceResponse(body);
  if (appearance === null) return appearanceFailure(model);
  const cursor = appearance.theme < model.themes.length ? appearance.theme : model.settingsCursor;
  const next = scopeOverlays({ ...model, appearance, appearanceBusy: false, appearanceClosing: false, settingsCursor: cursor,
    themes: highlightThemes(model.themes, cursor), settingsOpen: appearance.active });
  if (appearance.active) return appearanceDecision(next);
  if (model.navigationAfterSettings) return openNavigationAfterAppearance(next);
  return { ...appearanceDecision(next), closed: true };
}

function openNavigationAfterAppearance(model: Model): AppearanceDecision {
  const next = changeNavigation(model, { kind: "palette_open" });
  return { ...appearanceDecision(next), navigate: true };
}

function failedAppearance(model: Model): Model {
  return { ...model, appearanceBusy: false, appearance: { ...model.appearance,
    notice: asciiBytes("Appearance unavailable. Retry Save or Cancel before continuing.") } };
}

/// Dismissal never depends on a native round trip: a transaction that never
/// began has nothing to roll back, and a failed rollback must still release
/// Settings and the keyboard. A failed edit or Save keeps the preview open.
function dismissLocally(model: Model): AppearanceDecision {
  const next = scopeOverlays({ ...model, settingsOpen: false, appearanceBusy: false, appearanceClosing: false,
    appearance: initialAppearance() });
  if (model.navigationAfterSettings) return openNavigationAfterAppearance(next);
  return { ...appearanceDecision(next), closed: true };
}

function appearanceFailure(model: Model): AppearanceDecision {
  if (!model.settingsOpen) return appearanceDecision(model);
  if (model.appearanceClosing) return dismissLocally(model);
  return appearanceDecision(failedAppearance(model));
}

function dismissAppearance(model: Model): AppearanceDecision {
  if (!model.appearance.active && !model.appearanceBusy) return dismissLocally(model);
  return requestAppearance({ ...model, appearanceClosing: true }, 6, 0);
}

/// The placement menu command joins the open transaction so Cancel, Save and
/// the Settings selector all see it; with no transaction it is ignored.
function togglePreviewPlacement(model: Model): AppearanceDecision {
  if (!model.appearance.active) return appearanceDecision(model);
  return requestAppearance(model, 5, model.appearance.placement === 1 ? 0 : 1);
}

function previewTheme(model: Model, index: number): AppearanceDecision {
  if (!(index >= 0 && index < model.themes.length && index <= 32)) return appearanceDecision(model);
  const cursor = Math.trunc(index);
  return requestAppearance({ ...model, settingsCursor: cursor, themes: highlightThemes(model.themes, cursor) }, 1, cursor);
}

function editAppearance(model: Model, msg: Msg): AppearanceDecision | null {
  switch (msg.kind) {
    case "settings_pick": return previewTheme(model, msg.index);
    case "settings_move": return model.settingsSection === 0 ? previewTheme(model, model.settingsCursor + (msg.delta >= 0 ? 1 : -1)) : appearanceDecision(model);
    case "settings_font": return requestAppearance(model, msg.direction > 0 ? 2 : 3, 0);
    case "settings_cursor": return requestAppearance(model, 4, msg.index);
    case "settings_placement": return requestAppearance(model, 5, msg.index);
    case "toggle_tab_placement": return togglePreviewPlacement(model);
    case "settings_commit": return requestAppearance(model, 7, 0);
    default: return null;
  }
}

function updateAppearance(model: Model, msg: Msg): AppearanceDecision | null {
  if (msg.kind === "settings_open") return openAppearance(model);
  if (msg.kind === "appearance_loaded") return loadedAppearance(model, msg.body);
  if (msg.kind === "appearance_failed") return appearanceFailure(model);
  if (!model.settingsOpen) return null;
  return updateOpenAppearance(model, msg);
}

function updateOpenAppearance(model: Model, msg: Msg): AppearanceDecision | null {
  if (msg.kind === "settings_section") return appearanceDecision({ ...model, settingsSection: msg.section });
  if (msg.kind === "settings_close") return dismissAppearance(model);
  if (msg.kind === "palette_open") return dismissAppearance({ ...model, navigationAfterSettings: true });
  const edited = editAppearance(model, msg);
  if (edited === null) return null;
  return model.appearanceBusy ? appearanceDecision(model) : edited;
}

export function update(incoming: Model, msg: Msg): Model | [Model, Cmd<Msg>] {
  // Go to Directory first: while it is open it owns Escape and the arrows.
  const directory = directoryTransition(incoming, msg);
  if (directory !== null) {
    const decided = directory.model;
    if (directory.request.length > 0 && directory.committed) return [decided, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.directory", directory.request, { key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed" }),
    ])];
    if (directory.request.length > 0) {
      return [decided, Cmd.request("cockpit.directory", directory.request, { key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed" })];
    }
    if (directory.committed) return [decided, Cmd.host("cockpit.committed", NO_BYTES)];
    return decided;
  }
  const model = displaceDirectory(incoming, msg);
  const result = resultTransition(model, msg);
  if (result !== null) {
    if (result.request.length === 0) return result.model;
    return [result.model, Cmd.request("cockpit.command-results", result.request, {
      key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
    })];
  }
  const appearance = updateAppearance(model, msg);
  if (appearance !== null) {
    const next = appearance.model;
    if (appearance.opening) return [next, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.appearance", appearance.request, { key: "cockpit-appearance", ok: "appearance_loaded", err: "appearance_failed" }),
    ])];
    if (appearance.navigate) return [next, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.navigation", scopedNavigationRequest(next), {
        key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
      }),
    ])];
    if (appearance.closed) return [next, Cmd.host("cockpit.committed", NO_BYTES)];
    if (appearance.request.length === 0) return next;
    return [next, Cmd.request("cockpit.appearance", appearance.request, { key: "cockpit-appearance", ok: "appearance_loaded", err: "appearance_failed" })];
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
    case "host_open": {
      if (model.hostOpen) return model;
      return [openHost(model), Cmd.batch([
        Cmd.host("cockpit.committed", NO_BYTES),
        Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_STATUS, NO_BYTES), {
          key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
        }),
      ])];
    }
    case "host_close":
      if (!model.hostOpen) return model;
      return [scopeOverlays({ ...model, hostOpen: false, hostAwaiting: false }), Cmd.host("cockpit.committed", NO_BYTES)];
    case "host_edit":
      return editHost(model, msg.edit);
    case "host_submit": {
      if (!model.hostOpen || model.hostBusy) return model;
      if (model.hostQuery.length === 0) {
        return { ...model, hostNotice: asciiBytes("Enter a registered host, e.g. mini or me@mini") };
      }
      return [
        { ...model, hostBusy: true, hostAwaiting: true, hostNotice: joinBytes(asciiBytes("Connecting to "), model.hostQuery, asciiBytes("...")) },
        Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_CONNECT, model.hostQuery), {
          key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
        }),
      ];
    }
    case "host_local": {
      if (!model.hostOpen || model.hostBusy) return model;
      return [
        { ...model, hostBusy: true, hostAwaiting: true, hostNotice: asciiBytes("Returning to this Mac...") },
        Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_LOCAL, NO_BYTES), {
          key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
        }),
      ];
    }
    case "host_disconnect": {
      if (!model.hostOpen || model.hostBusy) return model;
      return [
        { ...model, hostBusy: true, hostAwaiting: true, hostNotice: asciiBytes("Disconnecting the remote host...") },
        Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_DISCONNECT, NO_BYTES), {
          key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
        }),
      ];
    }
    case "remote_loaded": {
      const next = receiveRemote(model, msg.body);
      if (model.hostOpen && !next.hostOpen) return [next, Cmd.host("cockpit.committed", NO_BYTES)];
      return next;
    }
    case "remote_failed":
      return { ...model, hostBusy: false, hostAwaiting: false, hostNotice: asciiBytes("Connection status unavailable. Try again.") };
    case "window_closed": {
      const window = msg.window;
      if (!(window >= 1 && window <= 4)) return model;
      return [model, Cmd.host("cockpit.intent", intent(9, model.engineRevision, 0, Math.trunc(window)))];
    }
    case "toggle_tab_placement": {
      const placement: TabPlacement = model.tabPlacement === "top" ? "side" : "top";
      return [
        { ...model, tabPlacement: placement },
        Cmd.host("cockpit.intent", intent(4, model.engineRevision, placement === "side" ? 1 : 0, 255)),
      ];
    }
    case "palette_open":
    case "palette_edit":
    case "palette_scope":
    case "palette_move":
    case "palette_previous":
    case "palette_next":
    case "palette_retry": {
      const next = changeNavigation(model, msg);
      if (next === model || !next.paletteLoading) return next;
      return [next, Cmd.batch([
        Cmd.host("cockpit.committed", NO_BYTES),
        Cmd.request("cockpit.navigation", scopedNavigationRequest(next), {
          key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
        }),
      ])];
    }
    case "palette_close":
      return [closePalette({ ...model, hostOpen: false, hostAwaiting: false }), Cmd.host("cockpit.committed", NO_BYTES)];
    case "palette_submit":
    case "palette_pick": {
      const target = navigationTarget(model, msg);
      if (target.length === 0) return model;
      const host = navigationHostFilter(target);
      if (host !== null) {
        const filtered = hostNavigation(model, host);
        return [filtered, Cmd.request("cockpit.navigation", scopedNavigationRequest(filtered), {
          key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
        })];
      }
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
      if (model.paletteOffset > 0) return [requestNavigation(model, 0), Cmd.request("cockpit.navigation", scopedNavigationRequest(requestNavigation(model, 0)), {
        key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
      })];
      return { ...model, paletteLoading: false, paletteNotice: asciiBytes("Workspace unavailable. Retry to refresh.") };
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
        railRows: railRows(mainTabs),
        workspaceLabel: projected.currentSession.length > 0 ? projected.currentSession : asciiBytes("Local workspace"),
        coordinatorEndpoint: projected.coordinatorEndpoint,
        connectionDetail: projected.connectionDetail,
        window1RailRows: railRows(w1.tabs),
        window2RailRows: railRows(w2.tabs),
        window3RailRows: railRows(w3.tabs),
        window4RailRows: railRows(w4.tabs),
        tabWidth,
        hasOverflow: hidden > 0,
        overflowLabel: hidden > 0 ? overflowLabel(hidden) : new Uint8Array(0),
        selectedTab,
        tabPlacement: projected.tabPlacement === 1 ? "side" : "top",
        engineConnected: true,
        engineSequence: projected.sequence,
        engineRevision: projected.revision,
        canReconnect: projected.connection === 3,
        lastConnection: projected.connection >= 0 && projected.connection <= 255 ? Math.trunc(projected.connection) : 255,
        connectionStatus: remoteConnectionStatus(model, windowStatus(projected.connection, projected.terminalStates[0], refusedMask !== 0)),
        window1Status: windowStatus(projected.connection, projected.terminalStates[1], refusedMask !== 0),
        window2Status: windowStatus(projected.connection, projected.terminalStates[2], refusedMask !== 0),
        window3Status: windowStatus(projected.connection, projected.terminalStates[3], refusedMask !== 0),
        window4Status: windowStatus(projected.connection, projected.terminalStates[4], refusedMask !== 0),
        status: refusedMask === 0 ? asciiBytes("READY") : asciiBytes("ACTION REFUSED"),
      };
      // An open Go to Directory names a listing on the connection that just
      // moved: withdraw its rows, and list again once connected.
      const directoryMoved = model.dirOpen && model.lastConnection !== 255 && projected.connection !== model.lastConnection;
      const directoryRelists = directoryMoved && projected.connection === 2;
      const scoped = directoryMoved ? relistDirectory(scopeOverlays(synced), directoryRelists) : scopeOverlays(synced);
      // Remote status is asked for only when the connection moved (or a
      // Connect to Host is waiting on it), never once per snapshot.
      const askRemote = projected.connection !== model.lastConnection || model.hostAwaiting;
      if (!model.paletteOpen) {
        if (!askRemote) return scoped;
        if (directoryRelists) return [scoped, Cmd.batch([
          Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_STATUS, NO_BYTES), {
            key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
          }),
          Cmd.request("cockpit.directory", directoryRequest(DIR_KIND_OPEN, NO_DIRECTORY_REQUEST, 0, 0, NO_BYTES), {
            key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed",
          }),
        ])];
        return [scoped, Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_STATUS, NO_BYTES), {
          key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
        })];
      }
      if (!askRemote) {
        return [refreshNavigation(scoped), Cmd.request("cockpit.navigation", scopedNavigationRequest(scoped), {
          key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
        })];
      }
      return [refreshNavigation(scoped), Cmd.batch([
        Cmd.request("cockpit.navigation", scopedNavigationRequest(scoped), {
          key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
        }),
        Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_STATUS, NO_BYTES), {
          key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
        }),
      ])];    }
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
      // A listing Go to Directory waits on settles in the provider drain
      // that announced this invalidation; ask for its page with the snapshot.
      const pollDirectory = model.dirOpen && model.dirAwaiting;
      const directoryPoll = directoryRequest(DIR_KIND_PAGE, model.dirRequest, model.dirOffset, 0, model.dirQuery);
      if (read.request.length > 0 && pollDirectory) return [next, Cmd.batch([
        Cmd.request("cockpit.snapshot", NO_BYTES, {
          key: "cockpit-snapshot", ok: "snapshot_loaded", err: "snapshot_failed",
        }),
        Cmd.request("cockpit.command-results", read.request, {
          key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
        }),
        Cmd.request("cockpit.directory", directoryPoll, { key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed" }),
      ])];
      if (read.request.length > 0) return [next, Cmd.batch([
        Cmd.request("cockpit.snapshot", NO_BYTES, {
          key: "cockpit-snapshot", ok: "snapshot_loaded", err: "snapshot_failed",
        }),
        Cmd.request("cockpit.command-results", read.request, {
          key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
        }),
      ])];
      if (pollDirectory) return [next, Cmd.batch([
        Cmd.request("cockpit.snapshot", NO_BYTES, {
          key: "cockpit-snapshot", ok: "snapshot_loaded", err: "snapshot_failed",
        }),
        Cmd.request("cockpit.directory", directoryPoll, { key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed" }),
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
