export const ENGINE_CHANNEL_KEY = 0x434f434b0001;
export const PROTOCOL_VERSION = 1;

const STATE_INVALIDATED = 1;
const SNAPSHOT = 2;
const INVALIDATION_LENGTH = 18;

export interface WireU64 {
  readonly hi: number;
  readonly lo: number;
}

export interface Invalidation {
  readonly sequence: WireU64;
  readonly revision: WireU64;
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

export function snapshot(bytes: Uint8Array): EngineSnapshot | null {
  if (bytes.length < 28) return null;
  if (bytes[0] !== PROTOCOL_VERSION || bytes[1] !== SNAPSHOT) return null;
  const count = bytes[20];
  const selected = bytes[21];
  // ScriptC proves every integer slot ahead of time: a byte read past the
  // end is NaN in TypeScript, so each value is fenced with an ordered
  // comparison before it may enter the model. The bounds are the wire's own
  // (u8 count, u32 id), not guesses.
  if (!(count >= 0 && count <= 255)) return null;
  if (!(selected >= 0 && selected <= 255)) return null;
  if (selected >= count && count !== 0) return null;
  const runStart = bytes[24];
  const runCount = bytes[25];
  const tabWidth = bytes[26] + bytes[27] * 256;
  if (!(runStart >= 0 && runStart <= 255) || !(runCount >= 0 && runCount <= 255)) return null;
  if (!(tabWidth >= 0 && tabWidth <= 65535)) return null;
  if (runStart + runCount > count) return null;
  const main = readTabs(bytes, 28, count, selected);
  if (main === null) return null;
  const tabs = main.tabs;
  let at = main.at;
  // The settings trailer.
  if (at + 1 > bytes.length) return null;
  const themeCount = bytes[at];
  if (!(themeCount >= 0 && themeCount <= 32)) return null;
  at += 1;
  const themes: ThemeEntry[] = [];
  for (let index = 0; index < themeCount; index += 1) {
    if (!(index >= 0 && index <= 32)) return null;
    if (at + 1 > bytes.length) return null;
    const nameLength = bytes[at];
    if (!(nameLength >= 1 && nameLength <= 32) || at + 1 + nameLength > bytes.length) return null;
    themes.push({ index, name: bytes.subarray(at + 1, at + 1 + nameLength) });
    at += 1 + nameLength;
  }
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
  // The secondary window sections.
  if (at + 1 > bytes.length) return null;
  const secondaryCount = bytes[at];
  if (!(secondaryCount >= 0 && secondaryCount <= 4)) return null;
  at += 1;
  const secondary: SecondaryWindow[] = [];
  for (let slot = 0; slot < secondaryCount; slot += 1) {
    if (at + 7 > bytes.length) return null;
    const index = bytes[at];
    const tabCount = bytes[at + 1];
    const selectedIn = bytes[at + 2];
    const runStart = bytes[at + 3];
    const runCount = bytes[at + 4];
    const tabWidth = bytes[at + 5] + bytes[at + 6] * 256;
    if (!(index >= 1 && index <= 4) || !(tabCount >= 0 && tabCount <= 255) || !(selectedIn >= 0 && selectedIn <= 255)) return null;
    if (!(runStart >= 0 && runStart <= 255) || !(runCount >= 0 && runCount <= 255) || !(tabWidth >= 0 && tabWidth <= 65535)) return null;
    if (selectedIn >= tabCount && tabCount !== 0) return null;
    if (runStart + runCount > tabCount) return null;
    const section = readTabs(bytes, at + 7, tabCount, selectedIn);
    if (section === null) return null;
    secondary.push({ index, selectedTab: selectedIn, runStart, runCount, tabWidth, tabs: section.tabs });
    at = section.at;
  }
  if (at !== bytes.length) return null;
  return {
    secondary,
    themes,
    activeTheme,
    configEnabled: (configFlags & 1) !== 0,
    configExists: (configFlags & 2) !== 0,
    configWritable: (configFlags & 4) !== 0,
    configProbed: (configFlags & 8) !== 0,
    configPath,
    sequence: readU64(bytes, 2),
    revision: readU64(bytes, 10),
    activeWindow: bytes[18],
    tabPlacement: bytes[19],
    selectedTab: selected,
    flags: bytes[22],
    runStart,
    runCount,
    tabWidth,
    tabs,
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
