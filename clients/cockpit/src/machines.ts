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
const EMPTY_MACHINE_ROW: MachineRow = {
  index: 0, role: 0, route: 0, state: 0, name: EMPTY, endpoint: EMPTY, session: EMPTY,
  message: EMPTY, status: EMPTY, action: EMPTY, target: EMPTY, highlighted: false,
  connected: false, canDisconnect: false, height: 0, canForget: false, disabled: true,
};
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

function machineRecord(bytes: Uint8Array, at: number, generation: number): MachineRecord | null {
  if (at + 7 > bytes.length) return null;
  const b0 = bytes.subarray(at, at + 1)[0];
  const b1 = bytes.subarray(at + 1, at + 2)[0];
  const b2 = bytes.subarray(at + 2, at + 3)[0];
  const b3 = bytes.subarray(at + 3, at + 4)[0];
  const rawRole = bytes.subarray(at + 4, at + 5)[0];
  const rawRoute = bytes.subarray(at + 5, at + 6)[0];
  const rawState = bytes.subarray(at + 6, at + 7)[0];
  const parsed = machineFields(bytes, at + 7);
  if (parsed === null) return null;
  if (b0 === undefined || b1 === undefined || b2 === undefined || b3 === undefined) return null;
  if (rawRole === undefined || rawRoute === undefined || rawState === undefined) return null;
  if (b0 >= 0 && b0 <= 9007199254740991 && b1 >= 0 && b1 <= 9007199254740991
      && b2 >= 0 && b2 <= 9007199254740991 && b3 >= 0 && b3 <= 9007199254740991
      && rawRole >= 0 && rawRole <= 2 && rawRoute >= 0 && rawRoute <= 5 && rawState >= 0 && rawState <= 4) {
    const index = Math.trunc(b0) | Math.trunc(b1) << 8 | Math.trunc(b2) << 16 | Math.trunc(b3) << 24;
    const role = Math.trunc(rawRole);
    const route = Math.trunc(rawRoute);
    const state = Math.trunc(rawState);
    const fields = parsed.values;
    const target = new Uint8Array(8);
    putU32(target, 0, generation); putU32(target, 4, index);
    const message = fields[3];
    const busy = state === 1 || state === 3;
    const connected = state === 2;
    return {
      row: {
        ...EMPTY_MACHINE_ROW,
        index: index >= 0 && index <= 9007199254740991 ? Math.trunc(index) : 0,
        role: role >= 0 && role <= 9007199254740991 ? Math.trunc(role) : 0,
        route: route >= 0 && route <= 9007199254740991 ? Math.trunc(route) : 0,
        state: state >= 0 && state <= 9007199254740991 ? Math.trunc(state) : 0,
        name: fields[0], endpoint: fields[1], session: fields[2], message,
        target, status: statusLabel(state),
        action: route >= 3 ? asciiBytes("Edit Configuration to repair this route") : actionLabel(state),
        highlighted: false, connected,
        canDisconnect: role !== 0 && connected,
        height: message.length > 0 ? 128 : 96,
        canForget: role !== 0 && !connected && !busy,
        disabled: route >= 3 || busy,
      },
      end: parsed.end,
    };
  }
  return null;
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

function copyMachine(row: MachineRow, highlighted: boolean): MachineRow {
  return {
    index: row.index >= 0 && row.index <= 9007199254740991 ? Math.trunc(row.index) : 0,
    role: row.role >= 0 && row.role <= 9007199254740991 ? Math.trunc(row.role) : 0,
    route: row.route >= 0 && row.route <= 9007199254740991 ? Math.trunc(row.route) : 0,
    state: row.state >= 0 && row.state <= 9007199254740991 ? Math.trunc(row.state) : 0,
    name: row.name, endpoint: row.endpoint, session: row.session, message: row.message,
    status: row.status, action: row.action, target: row.target, highlighted,
    connected: row.connected, canDisconnect: row.canDisconnect,
    height: row.height >= 0 && row.height <= 9007199254740991 ? Math.trunc(row.height) : 0,
    canForget: row.canForget, disabled: row.disabled,
  };
}

export function filterMachines(state: MachineState, query: Uint8Array): MachineState {
  const visible: MachineRow[] = [];
  let selected: Uint8Array = EMPTY;
  for (const row of state.rows) {
    if (row !== undefined && machineMatches(row, query)) {
      const highlighted = sameBytes(row.target, state.selected);
      if (highlighted) selected = row.target;
      visible.push(copyMachine(row, highlighted));
    }
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
