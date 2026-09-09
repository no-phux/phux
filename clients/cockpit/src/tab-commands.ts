import { type WireU64, sameU64, readU64, writeU32 } from "./protocol.ts";

export interface PendingTabCommand {
  readonly id: WireU64;
  readonly bytes: Uint8Array;
}

export interface TabCommandState {
  readonly nextId: WireU64;
  readonly queue: readonly PendingTabCommand[];
  readonly lastId: WireU64;
  /// idle / pending / applied / rejected / full / unknown / exhausted.
  readonly outcome: number;
}

export interface TabCommandDecision {
  readonly state: TabCommandState;
  /// Empty means no new request; helpers return data, never SDK commands.
  readonly request: Uint8Array;
}

const EMPTY = new Uint8Array(0);
const NO_COMMANDS: readonly PendingTabCommand[] = [];

export function initialTabCommands(): TabCommandState {
  return { nextId: { hi: 0, lo: 1 }, queue: NO_COMMANDS, lastId: { hi: 0, lo: 0 }, outcome: 0 };
}

function incrementId(id: WireU64): WireU64 {
  if (id.lo < 4294967295) return { hi: id.hi, lo: id.lo + 1 };
  if (id.hi < 4294967295) return { hi: id.hi + 1, lo: 0 };
  return { hi: 0, lo: 0 }; // Exhausted sentinel, never allocated.
}

function packet(id: WireU64, target: Uint8Array): Uint8Array {
  const bytes = new Uint8Array(32);
  bytes[0] = 1;
  bytes[1] = 1;
  writeU32(bytes, 2, id.lo);
  writeU32(bytes, 6, id.hi);
  for (let i = 0; i < target.length; i += 1) bytes[10 + i] = target[i];
  return bytes;
}

export function enqueueTabCommand(state: TabCommandState, target: Uint8Array): TabCommandDecision {
  if (target.length !== 22) return { state: { ...state, outcome: 3 }, request: EMPTY };
  if (state.queue.length >= 16) return { state: { ...state, outcome: 4 }, request: EMPTY };
  const id = state.nextId;
  if (id.hi === 0 && id.lo === 0) return { state: { ...state, outcome: 6 }, request: EMPTY };
  const bytes = packet(id, target);
  const queue: readonly PendingTabCommand[] = [...state.queue, { id, bytes }];
  return { state: { ...state, nextId: incrementId(id), queue, outcome: 1 }, request: state.queue.length === 0 ? bytes : EMPTY };
}

export function receiveTabReceipt(state: TabCommandState, bytes: Uint8Array): TabCommandDecision {
  if (state.queue.length === 0) return { state, request: EMPTY };
  if (!validReceipt(bytes)) return unknownTabCommand(state);
  const id = readU64(bytes, 3);
  if (!sameU64(id, state.queue[0].id)) return unknownTabCommand(state);
  const queue = state.queue.slice(1);
  return { state: { ...state, queue, lastId: id, outcome: bytes[1] === 1 ? 2 : 3 }, request: queue.length === 0 ? EMPTY : queue[0].bytes };
}

function validReceipt(bytes: Uint8Array): boolean {
  if (bytes.length !== 27 || bytes[0] !== 1) return false;
  if (bytes[1] === 1) return bytes[2] === 0;
  return bytes[1] === 2 && bytes[2] >= 1 && bytes[2] <= 3;
}

/// Delivery can fail after native application. Never resubmit it, or silently
/// send the queued dependent selections. A new explicit user action may start
/// a new command with a new ID; the uncertain ID is retained as lastId.
export function unknownTabCommand(state: TabCommandState): TabCommandDecision {
  const lastId = state.queue.length === 0 ? state.lastId : state.queue[0].id;
  return { state: { ...state, queue: NO_COMMANDS, lastId, outcome: 5 }, request: EMPTY };
}
