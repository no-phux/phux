import { Cmd, asciiBytes, windowDescriptor } from "@native-sdk/core";
import { type WindowDescriptor } from "@native-sdk/core/events";
import { applyTextInputEvent, type TextEditState, type TextInputEvent } from "@native-sdk/core/text";
import {
  ENGINE_CHANNEL_KEY,
  type WireU64,
  invalidation,
  intent,
  nextU64,
  sameU64,
  snapshot,
} from "./protocol.ts";

export interface Tab {
  readonly id: number;
  readonly index: number;
  readonly slot: number;
  readonly title: Uint8Array;
  readonly cwd: Uint8Array;
  readonly selected: boolean;
  readonly attention: boolean;
}

/// One row of the terminal switcher: the tab's position as the person reads
/// it off the strip, and its title, matched against the typed needle.
export interface SwitcherRow {
  readonly id: number;
  readonly index: number;
  readonly label: Uint8Array;
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
  readonly paletteOpen: boolean;
  readonly paletteQuery: Uint8Array;
  readonly paletteAnchor: number;
  readonly paletteFocus: number;
  readonly paletteRows: readonly SwitcherRow[];
  readonly paletteCursor: number;
  readonly settingsOpen: boolean;
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
  readonly window2Open: boolean;
  readonly window2Tabs: readonly Tab[];
  readonly window2TabWidth: number;
  readonly window2HasOverflow: boolean;
  readonly window2OverflowLabel: Uint8Array;
  readonly window3Open: boolean;
  readonly window3Tabs: readonly Tab[];
  readonly window3TabWidth: number;
  readonly window3HasOverflow: boolean;
  readonly window3OverflowLabel: Uint8Array;
  readonly window4Open: boolean;
  readonly window4Tabs: readonly Tab[];
  readonly window4TabWidth: number;
  readonly window4HasOverflow: boolean;
  readonly window4OverflowLabel: Uint8Array;
  readonly engineConnected: boolean;
  readonly engineSequence: WireU64;
  readonly engineRevision: WireU64;
  readonly status: Uint8Array;
}

export type Msg =
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
  | { readonly kind: "palette_pick"; readonly index: number }
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
  "selectedTab",
  "select_active_tab",
  "window_closed",
  "paletteAnchor",
  "paletteFocus",
  "paletteCursor",
  "settingsCursor",
  "palette_move",
  "settings_move",
  "engineConnected",
  "engineSequence",
  "engineRevision",
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

function lower(byte: number): number {
  return byte >= 65 && byte <= 90 ? byte + 32 : byte;
}

/// Case-insensitive ASCII substring, the shipping palette's rule
/// (workspace_projection.zig containsIgnoreCase).
function containsIgnoreCase(haystack: Uint8Array, needle: Uint8Array): boolean {
  if (needle.length === 0) return true;
  if (needle.length > haystack.length) return false;
  for (let start = 0; start + needle.length <= haystack.length; start += 1) {
    let matched = true;
    for (let at = 0; at < needle.length; at += 1) {
      if (lower(haystack[start + at]) !== lower(needle[at])) {
        matched = false;
        break;
      }
    }
    if (matched) return true;
  }
  return false;
}

function positionBytes(index: number): Uint8Array {
  return overflowLabel(index + 1).subarray(1);
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

const TWO_SPACES = asciiBytes("  ");
const NO_BYTES = new Uint8Array(0);
// Typed empties: a bare `[]` in a record literal is inferred as number[] and
// the record then fails to be a Model at the union boundary, at runtime.
const NO_ROWS: readonly SwitcherRow[] = [];
const NO_TABS: readonly Tab[] = [];
const NO_THEMES: readonly ThemeRow[] = [];

/// The switcher rows for a needle: every tab whose strip position or title
/// contains it, the shipping palette's rule minus the working directory,
/// which the snapshot does not carry yet. The cursor is clamped into the
/// filtered list so an Enter always lands on a row that is showing.
function switcherRows(tabs: readonly Tab[], needle: Uint8Array, cursor: number): readonly SwitcherRow[] {
  const rows: SwitcherRow[] = [];
  for (let index = 0; index < tabs.length; index += 1) {
    if (!(index >= 0 && index <= 255)) break;
    const tab = tabs[index];
    const position = positionBytes(index);
    if (!containsIgnoreCase(position, needle) && !containsIgnoreCase(tab.title, needle) && !containsIgnoreCase(tab.cwd, needle)) continue;
    rows.push({ id: tab.id, index, label: joinBytes(position, TWO_SPACES, tab.title), highlighted: false });
  }
  if (rows.length === 0) return rows;
  const at = cursor >= 0 && cursor < rows.length ? Math.trunc(cursor) : 0;
  return rows.map((row, i) => ({ ...row, highlighted: i === at }));
}

function paletteState(model: Model): TextEditState {
  return {
    text: model.paletteQuery,
    selection: { anchor: model.paletteAnchor, focus: model.paletteFocus },
    composition: null,
  };
}

function withSwitcher(model: Model, query: Uint8Array, anchor: number, focus: number, cursor: number): Model {
  // Caret and cursor positions enter integer slots: fenced to the query's
  // own bound and the row count, then stated whole.
  const a = anchor >= 0 && anchor <= 4096 ? Math.trunc(anchor) : 0;
  const f = focus >= 0 && focus <= 4096 ? Math.trunc(focus) : 0;
  const rows = switcherRows(model.tabs, query, cursor);
  const clamped = rows.length === 0 ? 0 : cursor >= 0 && cursor < rows.length && cursor <= 255 ? Math.trunc(cursor) : 0;
  return { ...model, paletteQuery: query, paletteAnchor: a, paletteFocus: f, paletteRows: rows, paletteCursor: clamped };
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

function stampSlots(tabs: readonly { readonly id: number; readonly index: number; readonly title: Uint8Array; readonly cwd: Uint8Array; readonly selected: boolean; readonly attention: boolean }[], window: number): readonly Tab[] {
  const out: Tab[] = [];
  const w = window >= 0 && window <= 4 ? Math.trunc(window) : 0;
  for (let i = 0; i < tabs.length; i += 1) {
    const t = tabs[i];
    const rawIndex = t.index;
    const rawId = t.id;
    if (!(rawIndex >= 0 && rawIndex <= 31) || !(rawId >= 1 && rawId <= 4294967295)) continue;
    const index = Math.trunc(rawIndex);
    const id = Math.trunc(rawId);
    out.push({ id, index, slot: w * 32 + index, title: t.title, cwd: t.cwd, selected: t.selected, attention: t.attention });
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

function windowState(index: number, section: { readonly selectedTab: number; readonly runStart: number; readonly runCount: number; readonly tabWidth: number; readonly tabs: readonly { readonly id: number; readonly index: number; readonly title: Uint8Array; readonly cwd: Uint8Array; readonly selected: boolean; readonly attention: boolean }[] } | null): WindowState {
  if (section === null) return closedWindow(index);
  const at = index >= 0 && index <= 4 ? Math.trunc(index) : 0;
  const tabs = stampSlots(section.tabs, at);
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

function findSection(sections: readonly { readonly index: number; readonly selectedTab: number; readonly runStart: number; readonly runCount: number; readonly tabWidth: number; readonly tabs: readonly { readonly id: number; readonly index: number; readonly title: Uint8Array; readonly cwd: Uint8Array; readonly selected: boolean; readonly attention: boolean }[] }[], index: number) {
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
      tabs: [{ id: 1, index: 0, slot: 0, title: asciiBytes("Terminal 1"), cwd: new Uint8Array(0), selected: true, attention: false }],
      visibleTabs: [{ id: 1, index: 0, slot: 0, title: asciiBytes("Terminal 1"), cwd: new Uint8Array(0), selected: true, attention: false }],
      tabWidth: 168,
      hasOverflow: false,
      overflowLabel: new Uint8Array(0),
      selectedTab: 0,
      tabPlacement: "top",
      paletteOpen: false,
      paletteQuery: new Uint8Array(0),
      paletteAnchor: 0,
      paletteFocus: 0,
      paletteRows: NO_ROWS,
      paletteCursor: 0,
      settingsOpen: false,
      themes: NO_THEMES,
      settingsCursor: 0,
      configExists: false,
      configNotice: new Uint8Array(0),
      window1Open: false,
      window1Tabs: NO_TABS,
      window1TabWidth: 168,
      window1HasOverflow: false,
      window1OverflowLabel: new Uint8Array(0),
      window2Open: false,
      window2Tabs: NO_TABS,
      window2TabWidth: 168,
      window2HasOverflow: false,
      window2OverflowLabel: new Uint8Array(0),
      window3Open: false,
      window3Tabs: NO_TABS,
      window3TabWidth: 168,
      window3HasOverflow: false,
      window3OverflowLabel: new Uint8Array(0),
      window4Open: false,
      window4Tabs: NO_TABS,
      window4TabWidth: 168,
      window4HasOverflow: false,
      window4OverflowLabel: new Uint8Array(0),
      engineConnected: false,
      engineSequence: ZERO_U64,
      engineRevision: ZERO_U64,
      status: asciiBytes("CONNECTING"),
    },
    Cmd.batch([
      Cmd.channelOpen(ENGINE_CHANNEL_KEY, { event: "engine_event" }),
      Cmd.request("cockpit.snapshot", new Uint8Array(0), {
        key: "cockpit-snapshot",
        ok: "snapshot_loaded",
        err: "snapshot_failed",
      }),
    ]),
  ];
}

function selectTab(tabs: readonly Tab[], selected: number): readonly Tab[] {
  return tabs.map((tab) => ({ ...tab, selected: tab.index === selected }));
}

export function update(model: Model, msg: Msg): Model | [Model, Cmd<Msg>] {
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
      // A tab pressed in a secondary window's chrome: the intent names that
      // window so it cannot land on whichever window is active.
      const slot = msg.slot;
      if (!(slot >= 0 && slot <= 159)) return model;
      let window = 0;
      let index = Math.trunc(slot);
      while (index >= 32) {
        index -= 32;
        window += 1;
      }
      return [model, Cmd.host("cockpit.intent", intent(1, model.engineRevision, index, window))];
    }
    case "new_terminal":
      return [model, Cmd.host("cockpit.intent", intent(2, model.engineRevision, 0, 0))];
    case "new_window":
      return [model, Cmd.host("cockpit.intent", intent(8, model.engineRevision, 0, 0))];
    case "window_closed": {
      const window = msg.window;
      if (!(window >= 1 && window <= 4)) return model;
      return [model, Cmd.host("cockpit.intent", intent(9, model.engineRevision, 0, Math.trunc(window)))];
    }
    case "close_selected_tab":
      return [model, Cmd.host("cockpit.intent", intent(3, model.engineRevision, model.selectedTab, 0))];
    case "toggle_tab_placement": {
      const placement: TabPlacement = model.tabPlacement === "top" ? "side" : "top";
      return [
        { ...model, tabPlacement: placement },
        Cmd.host("cockpit.intent", intent(4, model.engineRevision, placement === "side" ? 1 : 0, 0)),
      ];
    }
    case "palette_open":
      // Two modal surfaces cannot both own the keyboard; the one just asked
      // for wins. Opening is idempotent so a repeat never clears a needle.
      if (model.paletteOpen) return model;
      return withSwitcher({ ...model, paletteOpen: true, settingsOpen: false }, new Uint8Array(0), 0, 0, 0);
    case "palette_close":
      return { ...model, paletteOpen: false, paletteQuery: new Uint8Array(0), paletteRows: NO_ROWS, paletteCursor: 0 };
    case "palette_edit": {
      if (!model.paletteOpen) return model;
      const next = applyTextInputEvent(paletteState(model), msg.edit, 96);
      if (next === null) return model;
      return withSwitcher(model, next.text, next.selection.anchor, next.selection.focus, 0);
    }
    case "palette_move": {
      if (!model.paletteOpen || model.paletteRows.length === 0) return model;
      const step = msg.delta >= 0 ? 1 : -1;
      const last = model.paletteRows.length - 1;
      const raw = model.paletteCursor + step < 0 ? 0 : model.paletteCursor + step > last ? last : model.paletteCursor + step;
      return withSwitcher(model, model.paletteQuery, model.paletteAnchor, model.paletteFocus, raw);
    }
    case "palette_submit": {
      if (!model.paletteOpen) return model;
      const closed: Model = { ...model, paletteOpen: false, paletteQuery: new Uint8Array(0), paletteRows: NO_ROWS, paletteCursor: 0 };
      // A commit with nothing matching still dismisses: an Enter that found
      // nothing and left the switcher up reads as a stuck keyboard.
      if (model.paletteRows.length === 0) return closed;
      const row = model.paletteRows[model.paletteCursor];
      const picked = row.index;
      const selected: Model = { ...closed, tabs: selectTab(model.tabs, picked), visibleTabs: selectTab(model.visibleTabs, picked), selectedTab: picked };
      return [selected, Cmd.host("cockpit.intent", intent(1, model.engineRevision, picked, 0))];
    }
    case "palette_pick": {
      if (!model.paletteOpen) return model;
      const index = msg.index;
      if (!(index >= 0 && index < model.tabs.length)) return model;
      const picked = Math.trunc(index);
      const chosen: Model = {
        ...model,
        paletteOpen: false,
        paletteQuery: new Uint8Array(0),
        paletteRows: NO_ROWS,
        paletteCursor: 0,
        tabs: selectTab(model.tabs, picked),
        visibleTabs: selectTab(model.visibleTabs, picked),
        selectedTab: picked,
      };
      return [chosen, Cmd.host("cockpit.intent", intent(1, model.engineRevision, picked, 0))];
    }
    case "settings_open": {
      if (model.settingsOpen) return model;
      // The probe is the engine's, once per opening, exactly as the shipping
      // app asks the disk once when the panel opens and never per frame.
      return [
        { ...model, settingsOpen: true, paletteOpen: false },
        Cmd.host("cockpit.intent", intent(7, model.engineRevision, 0, 0)),
      ];
    }
    case "settings_close":
      return { ...model, settingsOpen: false };
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
        { ...model, settingsOpen: false },
        Cmd.host("cockpit.intent", intent(5, model.engineRevision, model.settingsCursor, 0)),
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
        return { ...model, engineConnected: false, status: asciiBytes("BAD SNAPSHOT") };
      }
      // Bits 0..4 are the engine model's own limit and write refusals; bit 7
      // is the seam's: the last intent named a revision the engine had left.
      const refusedMask = projected.flags & 159;
      const rawSelected = projected.selectedTab;
      if (!(rawSelected >= 0 && rawSelected <= 255)) {
        return { ...model, engineConnected: false, status: asciiBytes("BAD SNAPSHOT") };
      }
      const selectedTab = Math.trunc(rawSelected);
      const rawWidth = projected.tabWidth;
      if (!(rawWidth >= 0 && rawWidth <= 65535)) {
        return { ...model, engineConnected: false, status: asciiBytes("BAD SNAPSHOT") };
      }
      const tabWidth = Math.trunc(rawWidth);
      const hidden = projected.tabs.length - projected.runCount;
      const active = projected.activeTheme;
      const cursor = model.settingsOpen ? model.settingsCursor : active >= 0 && active <= 32 && active < projected.themes.length ? Math.trunc(active) : 0;
      const refused = (projected.flags & 8) !== 0;
      const mainTabs = stampSlots(projected.tabs, 0);
      const w1 = windowState(1, findSection(projected.secondary, 1));
      const w2 = windowState(2, findSection(projected.secondary, 2));
      const w3 = windowState(3, findSection(projected.secondary, 3));
      const w4 = windowState(4, findSection(projected.secondary, 4));
      // The width crosses a record into an integer slot; the proof is
      // restated at the boundary, once per slot.
      const width1 = w1.tabWidth >= 0 && w1.tabWidth <= 65535 ? Math.trunc(w1.tabWidth) : 168;
      const width2 = w2.tabWidth >= 0 && w2.tabWidth <= 65535 ? Math.trunc(w2.tabWidth) : 168;
      const width3 = w3.tabWidth >= 0 && w3.tabWidth <= 65535 ? Math.trunc(w3.tabWidth) : 168;
      const width4 = w4.tabWidth >= 0 && w4.tabWidth <= 65535 ? Math.trunc(w4.tabWidth) : 168;
      const synced: Model = {
        ...model,
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
        visibleTabs: sliceRun(mainTabs, projected.runStart, projected.runCount),
        tabWidth,
        hasOverflow: hidden > 0,
        overflowLabel: hidden > 0 ? overflowLabel(hidden) : new Uint8Array(0),
        selectedTab,
        tabPlacement: projected.tabPlacement === 1 ? "side" : "top",
        engineConnected: true,
        engineSequence: projected.sequence,
        engineRevision: projected.revision,
        status: refusedMask === 0 ? asciiBytes("READY") : asciiBytes("ACTION REFUSED"),
      };
      return model.paletteOpen
        ? withSwitcher(synced, model.paletteQuery, model.paletteAnchor, model.paletteFocus, model.paletteCursor)
        : synced;
    }
    case "snapshot_failed":
      return { ...model, engineConnected: false, status: asciiBytes("ENGINE UNAVAILABLE") };
    case "engine_event": {
      if (msg.state !== "data") {
        return {
          ...model,
          engineConnected: false,
          status: msg.state === "rejected" ? asciiBytes("ENGINE REFUSED") : asciiBytes("ENGINE CLOSED"),
        };
      }
      const event = invalidation(msg.bytes);
      if (event === null) return { ...model, status: asciiBytes("ENGINE PROTOCOL ERROR") };
      const contiguous = sameU64(event.sequence, nextU64(model.engineSequence));
      const next = {
        ...model,
        engineSequence: event.sequence,
        status: contiguous || sameU64(model.engineSequence, ZERO_U64)
          ? asciiBytes("SYNCING")
          : asciiBytes("RESYNCING"),
      };
      return [
        next,
        Cmd.request("cockpit.snapshot", new Uint8Array(0), {
          key: "cockpit-snapshot",
          ok: "snapshot_loaded",
          err: "snapshot_failed",
        }),
      ];
    }
  }
}
