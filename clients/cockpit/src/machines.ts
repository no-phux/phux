import { asciiBytes } from "@native-sdk/core";
import { containsQuery } from "./commands.ts";
import { sameBytes } from "./protocol.ts";

export interface MachineRow {
  readonly index: number;
  readonly role: number;
  readonly route: number;
  readonly state: number;
  readonly name: Uint8Array;
  readonly endpoint: Uint8Array;
  readonly session: Uint8Array;
  readonly message: Uint8Array;
  readonly status: Uint8Array;
  readonly action: Uint8Array;
  readonly target: Uint8Array;
  readonly highlighted: boolean;
  readonly connected: boolean;
  readonly canDisconnect: boolean;
  readonly height: number;
  readonly canForget: boolean;
  readonly disabled: boolean;
}

export interface MachinePage {
  readonly requestId: number;
  readonly generation: number;
  readonly total: number;
  readonly first: number;
  readonly status: number;
  readonly message: Uint8Array;
  readonly rows: readonly MachineRow[];
}

export interface MachineState {
  readonly operation: number;
  readonly browseToken: Uint8Array;
  readonly statusFirst: number;
  readonly statusDirty: boolean;
  readonly requestId: number;
  readonly generation: number;
  readonly total: number;
  readonly rows: readonly MachineRow[];
  readonly visible: readonly MachineRow[];
  readonly selected: Uint8Array;
  readonly notice: Uint8Array;
  readonly loading: boolean;
  readonly failed: boolean;
  readonly hasMore: boolean;
  readonly forgetTarget: Uint8Array;
  readonly forgetName: Uint8Array;
}

const EMPTY = new Uint8Array(0);
const NO_MACHINES: readonly MachineRow[] = [];

export function initialMachines(): MachineState {
  return { operation: 0, browseToken: EMPTY, statusFirst: 0, statusDirty: false, requestId: 0, generation: 0, total: 0, rows: NO_MACHINES, visible: NO_MACHINES,
    selected: EMPTY, notice: EMPTY, loading: false, failed: false, hasMore: false, forgetTarget: EMPTY, forgetName: EMPTY };
}

function u32(bytes: Uint8Array, at: number): number {
  return bytes[at] + bytes[at + 1] * 256 + bytes[at + 2] * 65536 + bytes[at + 3] * 16777216;
}

function putU32(out: Uint8Array, at: number, value: number): void {
  out[at] = value % 256;
  out[at + 1] = Math.floor(value / 256) % 256;
  out[at + 2] = Math.floor(value / 65536) % 256;
  out[at + 3] = Math.floor(value / 16777216) % 256;
}

export function machineRequest(state: MachineState, operation: number, target: Uint8Array): Uint8Array {
  const out = new Uint8Array(16);
  out[0] = 1; out[1] = operation;
  putU32(out, 2, state.requestId);
  putU32(out, 6, state.generation);
  if (operation === 1) putU32(out, 10, state.rows.length);
  if (target.length === 8) {
    for (let at = 0; at < 8; at += 1) out[6 + at] = target[at];
  }
  out[14] = 64;
  return out;
}

export function requestMachines(state: MachineState, operation: number): MachineState {
  const requestId = (state.requestId + 1) % 4294967296;
  const action = operation >= 0 && operation <= 8 ? Math.trunc(operation) : 0;
  return { ...state, operation: action, requestId: requestId >= 0 && requestId <= 4294967295 ? Math.trunc(requestId) : 0,
    statusFirst: operation === 8 ? state.statusFirst : 0, statusDirty: operation === 0 ? false : state.statusDirty,
    loading: true, notice: asciiBytes("Refreshing machines...") };
}

/// Internal operation 8 reads existing inventory pages without refreshing the
/// registry generation. The native wire operation is still Page (1).
export function machineStatusRequest(state: MachineState): Uint8Array {
  const request = machineRequest(state, 1, EMPTY);
  putU32(request, 10, state.statusFirst);
  return request;
}

interface Field { readonly text: Uint8Array; readonly end: number; }
function field(bytes: Uint8Array, at: number): Field | null {
  if (at + 2 > bytes.length) return null;
  const end = at + 2 + bytes[at] + bytes[at + 1] * 256;
  if (end > bytes.length) return null;
  return { text: bytes.slice(at + 2, end), end };
}

function statusLabel(state: number): Uint8Array {
  if (state === 1) return asciiBytes("Connecting");
  if (state === 2) return asciiBytes("Connected");
  if (state === 3) return asciiBytes("Reconnecting");
  if (state === 4) return asciiBytes("Failed");
  return asciiBytes("Not connected");
}

function actionLabel(state: number): Uint8Array {
  if (state === 2) return asciiBytes("Browse Sessions");
  if (state === 4) return asciiBytes("Retry");
  if (state === 1 || state === 3) return asciiBytes("Connecting...");
  return asciiBytes("Connect");
}

interface MachineRecord { readonly row: MachineRow; readonly end: number; }
interface MachineFields { readonly values: readonly Uint8Array[]; readonly end: number; }

function machineFields(bytes: Uint8Array, at: number): MachineFields | null {
  const values: Uint8Array[] = [];
  let end = at;
  for (let i = 0; i < 4; i += 1) {
    const value = field(bytes, end);
    if (value === null) return null;
    values.push(value.text); end = value.end;
  }
  return { values, end };
}

function machineCapabilities(row: MachineRow): MachineRow {
  const busy = row.state === 1 || row.state === 3;
  return { ...row, canDisconnect: row.role !== 0 && row.connected,
    canForget: row.role !== 0 && !row.connected && !busy,
    height: row.message.length > 0 ? 128 : 96,
    disabled: row.route >= 3 || busy,
    action: row.route >= 3 ? asciiBytes("Edit Configuration to repair this route") : row.action };
}

function machineRecord(bytes: Uint8Array, at: number, generation: number): MachineRecord | null {
  if (at + 7 > bytes.length) return null;
  const rawIndex = u32(bytes, at);
  const rawRole = bytes[at + 4]; const rawRoute = bytes[at + 5]; const rawState = bytes[at + 6];
  const index = rawIndex >= 0 && rawIndex <= 4294967295 ? Math.trunc(rawIndex) : 0;
  // The complete seven-byte header was checked above. Unsigned conversion
  // carries its byte-valued integer proof into the AOT row record.
  const role = rawRole >>> 0;
  const route = rawRoute >>> 0;
  const state = rawState >>> 0;
  if (role > 2) return null;
  if (route > 5) return null;
  if (state > 4) return null;
  const parsed = machineFields(bytes, at + 7);
  if (parsed === null) return null;
  const fields = parsed.values;
  const target = new Uint8Array(8);
  putU32(target, 0, generation); putU32(target, 4, index);
  const row: MachineRow = { index, role, route, state, name: fields[0], endpoint: fields[1], session: fields[2], message: fields[3],
    target, status: statusLabel(state), action: actionLabel(state), highlighted: false, connected: state === 2,
    canDisconnect: false, height: 96,
    canForget: false, disabled: false };
  return { row: machineCapabilities(row), end: parsed.end };
}

export function machinePage(bytes: Uint8Array): MachinePage | null {
  if (bytes.length < 22 || bytes.length > 65536) return null;
  if (bytes[0] !== 1 || bytes[1] > 3) return null;
  const message = field(bytes, 20);
  if (message === null) return null;
  const count = bytes[18] + bytes[19] * 256;
  const generation = u32(bytes, 6);
  const rows: MachineRow[] = [];
  let at = message.end;
  for (let i = 0; i < count; i += 1) {
    const record = machineRecord(bytes, at, generation);
    if (record === null) return null;
    rows.push(record.row); at = record.end;
  }
  if (at !== bytes.length) return null;
  return { requestId: u32(bytes, 2), generation, total: u32(bytes, 10), first: u32(bytes, 14), status: bytes[1], message: message.text, rows };
}

function machineMatches(row: MachineRow, query: Uint8Array): boolean {
  return containsQuery(row.name, query) || containsQuery(row.endpoint, query) || containsQuery(row.session, query);
}

export function filterMachines(state: MachineState, query: Uint8Array): MachineState {
  const visible: MachineRow[] = [];
  let selected: Uint8Array = EMPTY;
  for (const row of state.rows) {
    if (!machineMatches(row, query)) continue;
    const highlighted = sameBytes(row.target, state.selected);
    if (highlighted) selected = row.target;
    visible.push({ ...row, highlighted });
  }
  return { ...state, visible, selected };
}

function sameMachine(a: MachineRow, b: MachineRow): boolean {
  return a.role === b.role && sameBytes(a.name, b.name) && sameBytes(a.endpoint, b.endpoint) && sameBytes(a.session, b.session);
}

function restoredSelection(state: MachineState, rows: readonly MachineRow[]): Uint8Array {
  for (const previous of state.rows) {
    if (!sameBytes(previous.target, state.selected)) continue;
    for (const row of rows) if (sameMachine(previous, row)) return row.target;
  }
  return EMPTY;
}

function updatedMachineRows(state: MachineState, page: MachinePage): readonly MachineRow[] {
  if (state.operation === 0 || state.operation === 5) return page.rows;
  const rows: MachineRow[] = [];
  for (const previous of state.rows) {
    let current = previous;
    for (const row of page.rows) if (row.index === previous.index) current = row;
    rows.push(current);
  }
  if (state.operation === 1) for (const row of page.rows) rows.push(row);
  return rows;
}

export function receiveMachines(state: MachineState, body: Uint8Array, query: Uint8Array): MachineState {
  const page = machinePage(body);
  if (page === null) return { ...state, loading: false, failed: true, notice: asciiBytes("Machines unavailable. Refresh to try again.") };
  if (page.requestId !== state.requestId) return state;
  if (!validStatusPage(state, page)) return { ...state, loading: false, failed: true, forgetTarget: EMPTY,
    notice: asciiBytes("Machine status changed unexpectedly. Refresh Machines before trying again.") };
  if (page.status !== 0 && page.rows.length === 0) return { ...state, loading: false, failed: true, notice: page.message, selected: EMPTY, forgetTarget: EMPTY };
  const rows = updatedMachineRows(state, page);
  const selected = restoredSelection(state, rows);
  const generation = page.generation >= 0 && page.generation <= 4294967295 ? Math.trunc(page.generation) : 0;
  const total = page.total >= 0 && page.total <= 4294967295 ? Math.trunc(page.total) : 0;
  const next = { ...state, generation, total, rows, selected,
    loading: false, failed: page.status !== 0, hasMore: rows.length < page.total, notice: page.message,
    forgetTarget: retainedForgetTarget(state, rows) };
  return filterMachines(advanceMachineStatus(next, page), query);
}

function validStatusPage(state: MachineState, page: MachinePage): boolean {
  if (state.operation !== 8 || page.status !== 0) return true;
  if (page.generation !== state.generation || page.first !== state.statusFirst) return false;
  return page.rows.length > 0 || page.first >= state.rows.length;
}

function retainedForgetTarget(state: MachineState, rows: readonly MachineRow[]): Uint8Array {
  if (state.operation !== 8) return EMPTY;
  for (const previous of state.rows) {
    if (!sameBytes(previous.target, state.forgetTarget)) continue;
    return stillForgettable(previous, rows) ? state.forgetTarget : EMPTY;
  }
  return EMPTY;
}

function stillForgettable(previous: MachineRow, rows: readonly MachineRow[]): boolean {
  for (const row of rows) {
    if (!sameBytes(row.target, previous.target)) continue;
    return row.canForget && sameMachine(previous, row);
  }
  return false;
}

function advanceMachineStatus(state: MachineState, page: MachinePage): MachineState {
  if (state.operation !== 8) return { ...state, statusFirst: 0 };
  const first = page.first + page.rows.length;
  if (first >= state.rows.length) return { ...state, statusFirst: 0 };
  const statusFirst = first >= 0 && first <= 4294967295 ? Math.trunc(first) : 0;
  return { ...state, statusFirst };
}

export function moveMachine(state: MachineState, delta: number, query: Uint8Array): MachineState {
  if (state.visible.length === 0) return state;
  let current = -1;
  for (let i = 0; i < state.visible.length; i += 1) if (sameBytes(state.visible[i].target, state.selected)) current = i;
  const next = Math.max(0, Math.min(state.visible.length - 1, current + delta));
  return filterMachines({ ...state, selected: state.visible[next].target }, query);
}

export function capturedMachine(state: MachineState, target: Uint8Array): MachineRow | null {
  if (state.failed) return null;
  if (state.loading && state.operation !== 8) return null;
  for (const row of state.rows) if (sameBytes(row.target, target)) return row;
  return null;
}
