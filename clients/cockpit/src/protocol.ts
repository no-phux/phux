export const ENGINE_CHANNEL_KEY = 0x434f434b0001;
export const PROTOCOL_VERSION = 1;

const STATE_INVALIDATED = 1;
const SNAPSHOT = 2;
const INVALIDATION_LENGTH = 18;
/// `ts_snapshot.ExtensionKind.agent_rows`.
const EXTENSION_AGENT_ROWS = 1;
const EXTENSION_TAB_CONTEXTS = 2;
const EXTENSION_NAVIGATION_CONTEXT = 3;
/// The Empty session state (empty_session.zig): which windows show it.
const EXTENSION_EMPTY_SESSION = 4;

/// A keep-empty session with no windows, as the snapshot offers it: a mask
/// of the windows showing its state (bit 0 is the main window), whether it
/// was picked in the switcher, whether New Tab is on its way, and its name
/// and host. `windows` is 0 when no record came.
export interface SnapshotEmptySession {
  readonly windows: number;
  readonly picked: boolean;
  readonly opening: boolean;
  readonly name: Uint8Array;
  readonly host: Uint8Array;
}

function noEmptySession(): SnapshotEmptySession {
  return { windows: 0, picked: false, opening: false, name: new Uint8Array(0), host: new Uint8Array(0) };
}

function readEmptySession(bytes: Uint8Array): SnapshotEmptySession | null {
  if (bytes.length < 4) return null;
  const windows = bytes[0];
  const flags = bytes[1];
  const nameLength = bytes[2];
  if (!(windows >= 1 && windows <= 31) || !(flags >= 0 && flags <= 3) || nameLength > 64) return null;
  const hostAt = 3 + nameLength;
  if (hostAt >= bytes.length) return null;
  const hostLength = bytes[hostAt];
  if (hostLength > 64 || hostAt + 1 + hostLength !== bytes.length) return null;
  return {
    windows: Math.trunc(windows),
    picked: (flags & 1) !== 0,
    opening: (flags & 2) !== 0,
    name: bytes.subarray(3, hostAt),
    host: bytes.subarray(hostAt + 1),
  };
}

const NO_AGENTS: readonly SnapshotAgentRow[] = [];

export interface WireU64 {
  readonly hi: number;
  readonly lo: number;
}

export interface Invalidation {
  readonly sequence: WireU64;
  readonly revision: WireU64;
}

/// One agent session running under a terminal, addressed by the tab it hangs
/// under. The session's own identity never crosses this seam: it addresses no
/// surface, and the core draws it as a child of a terminal it already has.
export interface SnapshotAgentRow {
  readonly window: number;
  readonly tab: number;
  /// 0 unknown, 1 working, 2 blocked, 3 done, 4 gone (agent_sessions.State).
  readonly state: number;
  readonly attention: boolean;
  readonly provider: Uint8Array;
}

export interface SnapshotTab {
  readonly id: number;
  readonly index: number;
  readonly title: Uint8Array;
  readonly cwd: Uint8Array;
  readonly selected: boolean;
  readonly attention: boolean;
  readonly target: Uint8Array;
}

export interface ThemeEntry {
  readonly index: number;
  readonly name: Uint8Array;
}

/// One open secondary window's section: which slot, its tabs, selection
/// and run. Presence is liveness.
export interface SecondaryWindow {
  readonly index: number;
  readonly selectedTab: number;
  readonly runStart: number;
  readonly runCount: number;
  readonly tabWidth: number;
  readonly tabs: readonly SnapshotTab[];
}

export interface EngineSnapshot extends Invalidation {
  readonly connection: number;
  readonly currentSession: Uint8Array;
  readonly coordinatorEndpoint: Uint8Array;
  readonly connectionDetail: Uint8Array;
  readonly terminalStates: Uint8Array;
  readonly activeWindow: number;
  readonly tabPlacement: number;
  readonly selectedTab: number;
  readonly flags: number;
  /// The run the band has room for, by the engine's projection: the first
  /// visible tab, how many, and the per-tab extent in points.
  readonly runStart: number;
  readonly runCount: number;
  readonly tabWidth: number;
  readonly tabs: readonly SnapshotTab[];
  /// The settings trailer: the builtin theme catalog, the theme in effect
  /// (255 when none is named), the config file's probed state, its path.
  readonly themes: readonly ThemeEntry[];
  readonly activeTheme: number;
  readonly configEnabled: boolean;
  readonly configExists: boolean;
  readonly configWritable: boolean;
  readonly configProbed: boolean;
  readonly configPath: Uint8Array;
  readonly secondary: readonly SecondaryWindow[];
  /// The agent rows the `agent_rows` extension record carried, empty when the
  /// snapshot carried none (which is also what an absent record means).
  readonly agents: readonly SnapshotAgentRow[];
  readonly emptySession: SnapshotEmptySession;
}

function readU32(bytes: Uint8Array, at: number): number {
  const value = bytes[at]
    + bytes[at + 1] * 256
    + bytes[at + 2] * 65536
    + bytes[at + 3] * 16777216;
  return value >= 0 && value <= 4294967295 ? Math.trunc(value) : 0;
}

export function readU64(bytes: Uint8Array, at: number): WireU64 {
  const low = readU32(bytes, at);
  const high = readU32(bytes, at + 4);
  return {
    lo: low >= 0 && low <= 4294967295 ? Math.trunc(low) : 0,
    hi: high >= 0 && high <= 4294967295 ? Math.trunc(high) : 0,
  };
}

export function sameU64(left: WireU64, right: WireU64): boolean {
  return left.hi === right.hi && left.lo === right.lo;
}

export function nextU64(value: WireU64): WireU64 {
  if (value.lo < 4294967295) return { hi: value.hi, lo: value.lo + 1 };
  return { hi: value.hi < 4294967295 ? value.hi + 1 : 0, lo: 0 };
}

export function invalidation(bytes: Uint8Array): Invalidation | null {
  if (bytes.length !== INVALIDATION_LENGTH) return null;
  if (bytes[0] !== PROTOCOL_VERSION || bytes[1] !== STATE_INVALIDATED) return null;
  return { sequence: readU64(bytes, 2), revision: readU64(bytes, 10) };
}

interface TabRecords {
  readonly tabs: readonly SnapshotTab[];
  readonly at: number;
}

/// `count` tab records from `start`: id, attention, bounded title. Shared by
/// the main section and every secondary window's.
function readTabs(bytes: Uint8Array, start: number, count: number, selected: number): TabRecords | null {
  const tabs: SnapshotTab[] = [];
  let at = start;
  for (let index = 0; index < count; index += 1) {
    const tab = readTab(bytes, at, index, selected);
    if (tab === null) return null;
    tabs.push(tab);
    at += 7 + tab.title.length + tab.cwd.length;
  }
  return { tabs, at };
}

function readTab(bytes: Uint8Array, at: number, index: number, selected: number): SnapshotTab | null {
  if (!(index >= 0 && index <= 255)) return null;
  if (at + 7 > bytes.length) return null;
  const rawId = readU32(bytes, at);
  if (!(rawId >= 1 && rawId <= 4294967295)) return null;
  const titleLength = bytes[at + 5];
  const cwdLength = bytes[at + 6];
  if (at + 7 + titleLength + cwdLength > bytes.length) return null;
  return {
    id: Math.trunc(rawId), index: Math.trunc(index),
    title: bytes.subarray(at + 7, at + 7 + titleLength),
    cwd: bytes.subarray(at + 7 + titleLength, at + 7 + titleLength + cwdLength),
    selected: index === selected, attention: bytes[at + 4] !== 0, target: new Uint8Array(0),
  };
}

interface ThemeRecords {
  readonly themes: readonly ThemeEntry[];
  readonly at: number;
}

function readThemeName(bytes: Uint8Array, at: number): Uint8Array | null {
  if (at + 1 > bytes.length) return null;
  const length = bytes[at];
  if (!(length >= 1 && length <= 32) || at + 1 + length > bytes.length) return null;
  return bytes.subarray(at + 1, at + 1 + length);
}

function readThemes(bytes: Uint8Array, start: number): ThemeRecords | null {
  let at = start;
  if (at + 1 > bytes.length) return null;
  const themeCount = bytes[at];
  if (!(themeCount >= 0 && themeCount <= 32)) return null;
  at += 1;
  const themes: ThemeEntry[] = [];
  for (let index = 0; index < themeCount; index += 1) {
    if (!(index >= 0 && index <= 32)) return null;
    const name = readThemeName(bytes, at);
    if (name === null) return null;
    themes.push({ index, name });
    at += 1 + name.length;
  }
  return { themes, at };
}

interface SettingsRecords {
  readonly activeTheme: number;
  readonly configFlags: number;
  readonly configPath: Uint8Array;
  readonly at: number;
}

function readSettings(bytes: Uint8Array, start: number): SettingsRecords | null {
  let at = start;
  if (at + 2 > bytes.length) return null;
  const activeTheme = bytes[at];
  const configFlags = bytes[at + 1];
  if (!(activeTheme >= 0 && activeTheme <= 255) || !(configFlags >= 0 && configFlags <= 255)) return null;
  at += 2;
  if (at + 1 > bytes.length) return null;
  const pathLength = bytes[at];
  if (!(pathLength >= 0 && pathLength <= 255) || at + 1 + pathLength > bytes.length) return null;
  const configPath = bytes.subarray(at + 1, at + 1 + pathLength);
  at += 1 + pathLength;
  return { activeTheme, configFlags, configPath, at };
}

function validRun(count: number, selected: number, first: number, shown: number): boolean {
  if (selected >= count && count !== 0) return false;
  return first + shown <= count;
}

function readWindow(bytes: Uint8Array, at: number): SecondaryWindow | null {
  if (at + 7 > bytes.length) return null;
  const index = bytes[at];
  const count = bytes[at + 1];
  const selected = bytes[at + 2];
  const first = bytes[at + 3];
  const shown = bytes[at + 4];
  const width = bytes[at + 5] + bytes[at + 6] * 256;
  if (!(index >= 1 && index <= 4)) return null;
  if (!validRun(count, selected, first, shown)) return null;
  const section = readTabs(bytes, at + 7, count, selected);
  if (section === null) return null;
  return { index, selectedTab: selected, runStart: first, runCount: shown, tabWidth: width, tabs: section.tabs };
}

interface SecondaryRecords {
  readonly windows: readonly SecondaryWindow[];
  readonly terminalStates: Uint8Array;
  readonly contexts: Uint8Array;
  readonly agents: readonly SnapshotAgentRow[];
  readonly navigation: NavigationSnapshotContext;
  readonly empty: SnapshotEmptySession;
}

/// The `agent_rows` payload: a row count, then `[window][tab][state][flags]
/// [provider length][provider]` per row.
function readAgentRows(bytes: Uint8Array, start: number, length: number): readonly SnapshotAgentRow[] | null {
  const end = start + length;
  if (length < 1 || end > bytes.length) return null;
  const count = bytes[start];
  if (!(count >= 0 && count <= 24)) return null;
  const rows: SnapshotAgentRow[] = [];
  let at = start + 1;
  for (let index = 0; index < count; index += 1) {
    if (at + 5 > end) return null;
    const window = bytes[at];
    const tab = bytes[at + 1];
    const state = bytes[at + 2];
    const flags = bytes[at + 3];
    const providerLength = bytes[at + 4];
    if (!(window >= 0 && window <= 4)) return null;
    if (!(tab >= 0 && tab <= 31)) return null;
    if (!(state >= 0 && state <= 4)) return null;
    if (!(flags >= 0 && flags <= 1)) return null;
    if (!(providerLength >= 0 && providerLength <= 255) || at + 5 + providerLength > end) return null;
    rows.push({
      window: Math.trunc(window),
      tab: Math.trunc(tab),
      state: Math.trunc(state),
      attention: flags !== 0,
      provider: bytes.subarray(at + 5, at + 5 + providerLength),
    });
    at += 5 + providerLength;
  }
  return at === end ? rows : null;
}

/// Extension records close the snapshot: a kind byte, a `u16` length, then
/// that many payload bytes. A kind this build does not know is stepped over
/// by its length rather than read as whatever follows it, so the seam grows
/// by adding kinds and never by moving bytes anybody already parses.
interface SnapshotExtensions {
  readonly agents: readonly SnapshotAgentRow[];
  readonly contexts: Uint8Array;
  readonly navigation: NavigationSnapshotContext;
  readonly empty: SnapshotEmptySession;
}

interface NavigationSnapshotContext {
  readonly currentSession: Uint8Array;
  readonly coordinatorEndpoint: Uint8Array;
  readonly connectionDetail: Uint8Array;
}

function emptyNavigationContext(): NavigationSnapshotContext {
  return { currentSession: new Uint8Array(0), coordinatorEndpoint: new Uint8Array(0), connectionDetail: new Uint8Array(0) };
}

function readNavigationSnapshotContext(bytes: Uint8Array): NavigationSnapshotContext | null {
  const fields: Uint8Array[] = [];
  let at = 0;
  for (const limit of [64, 160, 80]) {
    if (at >= bytes.length) return null;
    const length = bytes[at];
    if (length > limit || at + 1 + length > bytes.length) return null;
    fields.push(bytes.subarray(at + 1, at + 1 + length));
    at += 1 + length;
  }
  if (at !== bytes.length) return null;
  return { currentSession: fields[0], coordinatorEndpoint: fields[1], connectionDetail: fields[2] };
}

function snapshotExtension(previous: SnapshotExtensions, kind: number, payload: Uint8Array): SnapshotExtensions | null {
  if (kind === EXTENSION_AGENT_ROWS) {
    const agents = readAgentRows(payload, 0, payload.length);
    return agents === null ? null : { ...previous, agents };
  }
  if (kind === EXTENSION_TAB_CONTEXTS) {
    return payload.length === 80 ? { ...previous, contexts: payload } : null;
  }
  if (kind === EXTENSION_NAVIGATION_CONTEXT) {
    const navigation = readNavigationSnapshotContext(payload);
    return navigation === null ? null : { ...previous, navigation };
  }
  if (kind === EXTENSION_EMPTY_SESSION) {
    const empty = readEmptySession(payload);
    return empty === null ? null : { ...previous, empty };
  }
  return previous;
}

function readExtensions(bytes: Uint8Array, start: number): SnapshotExtensions | null {
  let result: SnapshotExtensions = { agents: NO_AGENTS, contexts: new Uint8Array(0), navigation: emptyNavigationContext(), empty: noEmptySession() };
  let at = start;
  while (at < bytes.length) {
    if (at + 3 > bytes.length) return null;
    const kind = bytes[at];
    const length = bytes[at + 1] + bytes[at + 2] * 256;
    if (!(length >= 0 && length <= 4096) || at + 3 + length > bytes.length) return null;
    const updated = snapshotExtension(result, kind, bytes.subarray(at + 3, at + 3 + length));
    if (updated === null) return null;
    result = updated;
    at += 3 + length;
  }
  return result;
}

function readSecondary(bytes: Uint8Array, start: number): SecondaryRecords | null {
  let at = start;
  if (at + 1 > bytes.length) return null;
  const secondaryCount = bytes[at];
  if (!(secondaryCount >= 0 && secondaryCount <= 4)) return null;
  at += 1;
  const secondary: SecondaryWindow[] = [];
  for (let slot = 0; slot < secondaryCount; slot += 1) {
    const section = readWindow(bytes, at);
    if (section === null) return null;
    secondary.push(section);
    at += 7;
    for (const tab of section.tabs) at += 7 + tab.title.length + tab.cwd.length;
  }
  return readSnapshotTrailer(bytes, at, secondary);
}

function readSnapshotTrailer(bytes: Uint8Array, at: number, secondary: readonly SecondaryWindow[]): SecondaryRecords | null {
  // Older snapshots carried no per-window terminal status trailer.
  if (at === bytes.length) return { windows: secondary, terminalStates: new Uint8Array(5), agents: NO_AGENTS, contexts: new Uint8Array(0), navigation: emptyNavigationContext(), empty: noEmptySession() };
  if (at + 5 > bytes.length) return null;
  const terminalStates = bytes.subarray(at, at + 5);
  for (const state of terminalStates) if (state > 7) return null;
  const extensions = readExtensions(bytes, at + 5);
  if (extensions === null) return null;
  return { windows: secondary, terminalStates, agents: extensions.agents, contexts: extensions.contexts, navigation: extensions.navigation, empty: extensions.empty };
}

function targetTabs(tabs: readonly SnapshotTab[], contexts: Uint8Array, window: number): readonly SnapshotTab[] {
  return tabs.map((tab) => ({ ...tab, target: tabTarget(contexts, window, tab.id) }));
}

/// Retain the native lifetime bytes from THIS projection in the painted event.
/// They are never reconstructed from a later model when a click is delivered.
function tabTarget(contexts: Uint8Array, window: number, id: number): Uint8Array {
  if (contexts.length !== 80) return new Uint8Array(0);
  const out = new Uint8Array(22);
  out[0] = 1;
  out[1] = window;
  for (let i = 0; i < 16; i += 1) out[2 + i] = contexts[window * 16 + i];
  writeU32(out, 18, id);
  return out;
}

function snapshotHeaderValid(bytes: Uint8Array): boolean {
  if (bytes.length < 28 || bytes.length > 4096) return false;
  if (bytes[0] !== PROTOCOL_VERSION || bytes[1] !== SNAPSHOT) return false;
  if (!(bytes[23] >= 0 && bytes[23] <= 4)) return false;
  return validRun(bytes[20], bytes[21], bytes[24], bytes[25]);
}

export function snapshot(bytes: Uint8Array): EngineSnapshot | null {
  if (!snapshotHeaderValid(bytes)) return null;
  const main = readTabs(bytes, 28, bytes[20], bytes[21]);
  if (main === null) return null;
  const catalog = readThemes(bytes, main.at);
  if (catalog === null) return null;
  const settings = readSettings(bytes, catalog.at);
  if (settings === null) return null;
  const secondary = readSecondary(bytes, settings.at);
  if (secondary === null) return null;
  return {
    connection: bytes[23],
    currentSession: secondary.navigation.currentSession,
    coordinatorEndpoint: secondary.navigation.coordinatorEndpoint,
    connectionDetail: secondary.navigation.connectionDetail,
    secondary: secondary.windows.map((window) => ({ ...window, tabs: targetTabs(window.tabs, secondary.contexts, window.index) })),
    terminalStates: secondary.terminalStates,
    themes: catalog.themes,
    activeTheme: settings.activeTheme,
    configEnabled: (settings.configFlags & 1) !== 0,
    configExists: (settings.configFlags & 2) !== 0,
    configWritable: (settings.configFlags & 4) !== 0,
    configProbed: (settings.configFlags & 8) !== 0,
    configPath: settings.configPath,
    sequence: readU64(bytes, 2),
    revision: readU64(bytes, 10),
    activeWindow: bytes[18],
    tabPlacement: bytes[19],
    selectedTab: bytes[21],
    flags: bytes[22],
    runStart: bytes[24],
    runCount: bytes[25],
    tabWidth: bytes[26] + bytes[27] * 256,
    tabs: targetTabs(main.tabs, secondary.contexts, 0),
    agents: secondary.agents,
    emptySession: secondary.empty,
  };
}

export function writeU32(bytes: Uint8Array, at: number, input: number): void {
  const value = input >= 0 && input <= 4294967295 ? Math.trunc(input) : 0;
  bytes[at] = value % 256;
  bytes[at + 1] = Math.floor(value / 256) % 256;
  bytes[at + 2] = Math.floor(value / 65536) % 256;
  bytes[at + 3] = Math.floor(value / 16777216) % 256;
}

export function intent(kind: number, revision: WireU64, argument: number, window: number): Uint8Array {
  const out = new Uint8Array(12);
  out[0] = PROTOCOL_VERSION;
  out[1] = kind;
  writeU32(out, 2, revision.lo);
  writeU32(out, 6, revision.hi);
  out[10] = argument;
  out[11] = window;
  return out;
}

export interface NavigationRow {
  readonly id: number;
  readonly index: number;
  readonly label: Uint8Array;
  readonly target: Uint8Array;
  readonly highlighted: boolean;
  readonly detail: Uint8Array;
  /// 0 open, 1 available, 2 session, 3 known host.
  readonly kind: number;
  readonly host: Uint8Array;
  readonly selectable: boolean;
  readonly current: boolean;
}

/// 0 all work, 1 sessions, 2 known terminal hosts, 3 exact raw host, 4 native windows,
/// 5 captured machine sessions (host field is opaque request-id/generation/row).
export type NavigationScope = number;

export interface NavigationPage {
  readonly revision: WireU64;
  readonly offset: number;
  readonly total: number;
  readonly query: Uint8Array;
  readonly scope: NavigationScope;
  readonly host: Uint8Array;
  readonly rows: readonly NavigationRow[];
}

/// The native compiler requires fixed arity; retain the original caller API.
export function navigationRequest(revision: WireU64, offset: number, query: Uint8Array): Uint8Array {
  return navigationScopedRequest(revision, offset, query, 0, new Uint8Array(0));
}

export function navigationScopedRequest(revision: WireU64, offset: number, query: Uint8Array, scope: NavigationScope, host: Uint8Array): Uint8Array {
  if (!validNavigationRequest(offset, query, scope, host)) return new Uint8Array(0);
  const scoped = scope !== 0;
  const out = new Uint8Array(13 + query.length + (scoped ? 2 + host.length : 0));
  out[0] = PROTOCOL_VERSION;
  out[1] = scoped ? 4 : 3;
  writeU32(out, 2, revision.lo);
  writeU32(out, 6, revision.hi);
  out[10] = offset % 256;
  out[11] = Math.floor(offset / 256);
  out[12] = query.length;
  for (let i = 0; i < query.length; i += 1) out[13 + i] = query[i];
  if (scoped) {
    out[13 + query.length] = scope;
    out[14 + query.length] = host.length;
    for (let i = 0; i < host.length; i += 1) out[15 + query.length + i] = host[i];
  }
  return out;
}

function validNavigationRequest(offset: number, query: Uint8Array, scope: number, host: Uint8Array): boolean {
  if (offset < 0 || offset > 65535 || offset !== Math.trunc(offset)) return false;
  if (scope < 0 || scope > 5 || scope !== Math.trunc(scope)) return false;
  if (query.length > 64 || host.length > 255) return false;
  return validNavigationFilter(scope, host.length);
}

function validNavigationFilter(scope: number, length: number): boolean {
  if (scope === 5) return length === 12;
  return scope === 3 || length === 0;
}

export function navigationIntent(revision: WireU64, index: number): Uint8Array {
  const out = intent(13, revision, 0, 0);
  out[10] = index % 256;
  out[11] = Math.floor(index / 256);
  return out;
}

interface NavigationRecord {
  readonly row: NavigationRow;
  readonly end: number;
}

/// Known-host rows carry `3, host_len, raw host` in the target slot: a filter
/// token, never catalog authority (catalog targets begin with 2).
const HOST_FILTER_TAG = 3;

/// The raw host a known-host filter token names, or null for any other target.
export function navigationHostFilter(target: Uint8Array): Uint8Array | null {
  if (target.length < 2 || target[0] !== HOST_FILTER_TAG) return null;
  return target.length === 2 + target[1] ? target.subarray(2) : null;
}

function validNavigationTarget(target: Uint8Array): boolean {
  if (target.length === 10 && target[0] === 4) return true;
  if (target.length === 22 && target[0] === 5) return true;
  if (navigationHostFilter(target) !== null) return true;
  return target.length >= 38 && target.length <= 298 && target[0] === 2;
}

function navigationRecord(bytes: Uint8Array, at: number, highlighted: boolean): NavigationRecord | null {
  if (at + 5 > bytes.length) return null;
  const rawIndex = bytes[at] + bytes[at + 1] * 256;
  const length = bytes[at + 2];
  const targetLength = bytes[at + 3] + bytes[at + 4] * 256;
  if (!(rawIndex >= 0 && rawIndex <= 65535)) return null;
  if (!(targetLength >= 2 && targetLength <= 298)) return null;
  const labelAt = at + 5 + targetLength;
  if (!(length >= 1 && length <= 240) || labelAt + length > bytes.length) return null;
  const target = bytes.slice(at + 5, labelAt);
  if (!validNavigationTarget(target)) return null;
  const index = Math.trunc(rawIndex);
  const row: NavigationRow = { id: index, index, target, label: bytes.subarray(labelAt, labelAt + length), highlighted,
    detail: new Uint8Array(0), kind: 0, host: new Uint8Array(0), selectable: true, current: false };
  return { row, end: labelAt + length };
}

function navigationRows(bytes: Uint8Array, start: number, count: number): readonly NavigationRow[] | null {
  const rows: NavigationRow[] = [];
  let at = start;
  for (let i = 0; i < count; i += 1) {
    const record = navigationRecord(bytes, at, i === 0);
    if (record === null) return null;
    rows.push(record.row);
    at = record.end;
  }
  if (at === bytes.length) return bytes[1] === 3 ? rows : null;
  return navigationMetadata(bytes, at, rows);
}

function navigationRowMetadata(bytes: Uint8Array, at: number, row: NavigationRow): NavigationRow | null {
  if (at + 3 > bytes.length) return null;
  const kind = bytes[at];
  const selectable = bytes[at + 1];
  const detailLength = bytes[at + 2];
  if (!(kind >= 0 && kind <= 5)) return null;
  if (selectable > 3 || detailLength > 160) return null;
  const end = at + 3 + detailLength;
  if (end > bytes.length) return null;
  // Exactly the host rows carry a filter token instead of catalog authority.
  const host = navigationHostFilter(row.target);
  if (!navigationKindMatchesTarget(kind, row.target, host !== null)) return null;
  return { ...row, kind: Math.trunc(kind), selectable: (selectable & 1) !== 0, current: (selectable & 2) !== 0, detail: bytes.subarray(at + 3, end), host: host ?? new Uint8Array(0) };
}

function navigationKindMatchesTarget(kind: number, target: Uint8Array, host: boolean): boolean {
  if ((kind === 3) !== host) return false;
  if ((kind === 4) !== (target[0] === 4)) return false;
  return (kind === 5) === (target[0] === 5);
}

function navigationMetadata(bytes: Uint8Array, start: number, rows: readonly NavigationRow[]): readonly NavigationRow[] | null {
  if (bytes[start] !== 0x4e) return null;
  let at = start + 1;
  const result: NavigationRow[] = [];
  for (const row of rows) {
    const detailed = navigationRowMetadata(bytes, at, row);
    if (detailed === null) return null;
    result.push(detailed);
    at += 3 + detailed.detail.length;
  }
  return at === bytes.length ? result : null;
}

function navigationHeaderValid(bytes: Uint8Array): boolean {
  if (bytes.length < 16 || bytes.length > 8192) return false;
  if (bytes[0] !== PROTOCOL_VERSION) return false;
  if (bytes[1] !== 3 && bytes[1] !== 4) return false;
  const queryLength = bytes[12];
  return queryLength >= 0 && queryLength <= 64 && 16 + queryLength <= bytes.length;
}

export function navigationPage(bytes: Uint8Array): NavigationPage | null {
  if (!navigationHeaderValid(bytes)) return null;
  const queryLength = bytes[12];
  const context = navigationContext(bytes, 13 + queryLength);
  if (context === null) return null;
  const at = context.at;
  const offset = bytes[10] + bytes[11] * 256;
  const total = bytes[at] + bytes[at + 1] * 256;
  const count = bytes[at + 2];
  const capacity = context.scope === 4 ? 16 : 4;
  if (context.scope !== 4 && bytes.length > 4096) return null;
  if (!validNavigationCount(count, offset, total, capacity)) return null;
  const rows = navigationRows(bytes, at + 3, count);
  if (rows === null) return null;
  return { revision: readU64(bytes, 2), offset, total, query: bytes.subarray(13, 13 + queryLength), scope: context.scope, host: context.host, rows };
}

function validNavigationCount(count: number, offset: number, total: number, capacity: number): boolean {
  if (!(count >= 0 && count <= capacity)) return false;
  if (offset + count > total) return false;
  return count === Math.min(capacity, total - offset);
}

interface NavigationContext { readonly scope: number; readonly host: Uint8Array; readonly at: number; }

function navigationContext(bytes: Uint8Array, at: number): NavigationContext | null {
  if (bytes[1] === 3) return { scope: 0, host: new Uint8Array(0), at };
  if (at + 2 > bytes.length) return null;
  const scope = bytes[at];
  const length = bytes[at + 1];
  if (!(scope >= 0 && scope <= 5)) return null;
  if (at + 5 + length > bytes.length) return null;
  if (!validNavigationFilter(scope, length)) return null;
  return { scope: Math.trunc(scope), host: bytes.subarray(at + 2, at + 2 + length), at: at + 2 + length };
}

export function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i += 1) {
    if (left[i] !== right[i]) return false;
  }
  return true;
}
