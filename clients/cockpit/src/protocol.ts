export const ENGINE_CHANNEL_KEY = 0x434f434b0001;
export const PROTOCOL_VERSION = 1;

const STATE_INVALIDATED = 1;
const SNAPSHOT = 2;
const INVALIDATION_LENGTH = 18;
/// `ts_snapshot.ExtensionKind.agent_rows`.
const EXTENSION_AGENT_ROWS = 1;

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
}

function readU32(bytes: Uint8Array, at: number): number {
  const value = bytes[at]
    + bytes[at + 1] * 256
    + bytes[at + 2] * 65536
    + bytes[at + 3] * 16777216;
  return value >= 0 && value <= 4294967295 ? Math.trunc(value) : 0;
}

function readU64(bytes: Uint8Array, at: number): WireU64 {
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
    if (!(index >= 0 && index <= 255)) return null;
    if (at + 7 > bytes.length) return null;
    const rawId = readU32(bytes, at);
    const titleLength = bytes[at + 5];
    const cwdLength = bytes[at + 6];
    if (!(rawId >= 1 && rawId <= 4294967295)) return null;
    const id = Math.trunc(rawId);
    if (!(titleLength >= 0 && titleLength <= 255)) return null;
    if (!(cwdLength >= 0 && cwdLength <= 255)) return null;
    if (at + 7 + titleLength + cwdLength > bytes.length) return null;
    tabs.push({
      id,
      index,
      title: bytes.subarray(at + 7, at + 7 + titleLength),
      cwd: bytes.subarray(at + 7 + titleLength, at + 7 + titleLength + cwdLength),
      selected: index === selected,
      attention: bytes[at + 4] !== 0,
    });
    at += 7 + titleLength + cwdLength;
  }
  return { tabs, at };
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
  readonly agents: readonly SnapshotAgentRow[];
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
function readExtensions(bytes: Uint8Array, start: number): readonly SnapshotAgentRow[] | null {
  let agents: readonly SnapshotAgentRow[] = NO_AGENTS;
  let at = start;
  while (at < bytes.length) {
    if (at + 3 > bytes.length) return null;
    const kind = bytes[at];
    const length = bytes[at + 1] + bytes[at + 2] * 256;
    if (!(length >= 0 && length <= 4096) || at + 3 + length > bytes.length) return null;
    if (kind === EXTENSION_AGENT_ROWS) {
      const rows = readAgentRows(bytes, at + 3, length);
      if (rows === null) return null;
      agents = rows;
    }
    at += 3 + length;
  }
  return agents;
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
  // Older snapshots carried no per-window terminal status trailer.
  if (at === bytes.length) return { windows: secondary, terminalStates: new Uint8Array(5), agents: NO_AGENTS };
  if (at + 5 > bytes.length) return null;
  const terminalStates = bytes.subarray(at, at + 5);
  for (const state of terminalStates) if (state > 7) return null;
  const agents = readExtensions(bytes, at + 5);
  if (agents === null) return null;
  return { windows: secondary, terminalStates, agents };
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
    secondary: secondary.windows,
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
    tabs: main.tabs,
    agents: secondary.agents,
  };
}

function writeU32(bytes: Uint8Array, at: number, input: number): void {
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
  readonly highlighted: boolean;
}

export interface NavigationPage {
  readonly revision: WireU64;
  readonly offset: number;
  readonly total: number;
  readonly query: Uint8Array;
  readonly rows: readonly NavigationRow[];
}

export function navigationRequest(revision: WireU64, offset: number, query: Uint8Array): Uint8Array {
  const out = new Uint8Array(13 + query.length);
  out[0] = PROTOCOL_VERSION;
  out[1] = 3;
  writeU32(out, 2, revision.lo);
  writeU32(out, 6, revision.hi);
  out[10] = offset % 256;
  out[11] = Math.floor(offset / 256);
  out[12] = query.length;
  for (let i = 0; i < query.length; i += 1) out[13 + i] = query[i];
  return out;
}

export function navigationIntent(revision: WireU64, index: number): Uint8Array {
  const out = intent(13, revision, 0, 0);
  out[10] = index % 256;
  out[11] = Math.floor(index / 256);
  return out;
}

function navigationRows(bytes: Uint8Array, start: number, count: number): readonly NavigationRow[] | null {
  const rows: NavigationRow[] = [];
  let at = start;
  for (let i = 0; i < count; i += 1) {
    if (at + 3 > bytes.length) return null;
    const rawIndex = bytes[at] + bytes[at + 1] * 256;
    const length = bytes[at + 2];
    if (!(rawIndex >= 0 && rawIndex <= 65535)) return null;
    if (!(length >= 1 && length <= 240) || at + 3 + length > bytes.length) return null;
    const index = Math.trunc(rawIndex);
    rows.push({ id: index, index, label: bytes.subarray(at + 3, at + 3 + length), highlighted: i === 0 });
    at += 3 + length;
  }
  return at === bytes.length ? rows : null;
}

function navigationHeaderValid(bytes: Uint8Array): boolean {
  if (bytes.length < 16 || bytes.length > 4096) return false;
  if (bytes[0] !== PROTOCOL_VERSION || bytes[1] !== 3) return false;
  const queryLength = bytes[12];
  return queryLength >= 0 && queryLength <= 64 && 16 + queryLength <= bytes.length;
}

export function navigationPage(bytes: Uint8Array): NavigationPage | null {
  if (!navigationHeaderValid(bytes)) return null;
  const queryLength = bytes[12];
  const at = 13 + queryLength;
  const offset = bytes[10] + bytes[11] * 256;
  const total = bytes[at] + bytes[at + 1] * 256;
  const count = bytes[at + 2];
  if (!(count >= 0 && count <= 4) || offset + count > total) return null;
  if (count !== Math.min(4, total - offset)) return null;
  const rows = navigationRows(bytes, at + 3, count);
  if (rows === null) return null;
  return { revision: readU64(bytes, 2), offset, total, query: bytes.subarray(13, at), rows };
}

export function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i += 1) {
    if (left[i] !== right[i]) return false;
  }
  return true;
}
