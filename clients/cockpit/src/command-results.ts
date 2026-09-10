import { asciiBytes } from "@native-sdk/core";
import { type WireU64, readU64, sameU64, writeU32 } from "./protocol.ts";

/// Eventual outcomes are independent from immediate command admission. Native
/// retains each result until a subsequent read acknowledges this exact command.
export interface CommandResult {
  readonly source: number;
  readonly id: WireU64;
  readonly operationEpoch: WireU64;
  readonly operationRequest: number;
  readonly mutationTicket: WireU64;
  readonly mutationOutcome: number;
  readonly targetSessionId: number;
  readonly attachmentRequest: number;
  readonly attachmentEpoch: WireU64;
  readonly placementRequest: number;
  readonly placementEpoch: WireU64;
  readonly errorDomain: number;
  readonly errorCode: number;
  readonly reason: number;
  readonly operation: number;
  readonly placement: number;
  readonly focus: number;
  readonly terminal: Uint8Array;
}

export interface CommandResults {
  readonly recent: readonly CommandResult[];
  readonly acknowledgement: Uint8Array;
  readonly loading: boolean;
  readonly refreshPending: boolean;
  readonly notice: Uint8Array;
  readonly deliveryNotice: Uint8Array;
}

export interface ResultDecision {
  readonly state: CommandResults;
  readonly request: Uint8Array;
}

const EMPTY = new Uint8Array(0);

export function initialCommandResults(): CommandResults {
  return { recent: [], acknowledgement: new Uint8Array([1, 0]), loading: false, refreshPending: false, notice: EMPTY, deliveryNotice: EMPTY };
}

export function requestCommandResults(state: CommandResults): ResultDecision {
  if (state.loading) return { state: { ...state, refreshPending: true }, request: EMPTY };
  return { state: { ...state, loading: true, refreshPending: false }, request: state.acknowledgement };
}

function validOutcomeTags(bytes: Uint8Array): boolean {
  if (bytes[1] < 1 || bytes[1] > 3) return false;
  if (bytes[2] < 1 || bytes[2] > 3) return false;
  if (bytes[3] < 1 || bytes[3] > 5) return false;
  return bytes[4] >= 1 && bytes[4] <= 3;
}

function validResult(bytes: Uint8Array): boolean {
  if (bytes.length < 74 || bytes[0] !== 1) return false;
  if (!validOutcomeTags(bytes)) return false;
  if (bytes[66] > 3 || bytes[67] !== 0) return false;
  const length = bytes[72] + bytes[73] * 256;
  return length <= 273 && bytes.length === 74 + length;
}

function readWord(bytes: Uint8Array, offset: number): number {
  return bytes[offset] + bytes[offset + 1] * 256 + bytes[offset + 2] * 65536 + bytes[offset + 3] * 16777216;
}

// Bounds must be local to assignments into the SDK's CommandResult model.
// Partial records and scalar helper returns lose their proofs at this boundary.
function readOutcomes(bytes: Uint8Array, result: CommandResult): CommandResult {
  const source = bytes[1];
  const operation = bytes[2];
  const placement = bytes[3];
  const focus = bytes[4];
  return {
    ...result,
    source: source >= 0 && source <= 255 ? Math.trunc(source) : 0,
    operation: operation >= 0 && operation <= 255 ? Math.trunc(operation) : 0,
    placement: placement >= 0 && placement <= 255 ? Math.trunc(placement) : 0,
    focus: focus >= 0 && focus <= 255 ? Math.trunc(focus) : 0,
  };
}

function readCorrelation(bytes: Uint8Array, id: WireU64): CommandResult {
  const operationRequest = readWord(bytes, 22);
  const attachmentRequest = readWord(bytes, 34);
  const placementRequest = readWord(bytes, 38);
  return {
    source: 0, operation: 0, placement: 0, focus: 0,
    mutationOutcome: 0, targetSessionId: 0, errorDomain: 0, errorCode: 0, reason: 0,
    mutationTicket: readU64(bytes, 26), terminal: bytes.slice(74),
    id, operationEpoch: readU64(bytes, 14),
    operationRequest: operationRequest >= 0 && operationRequest <= 4294967295 ? Math.trunc(operationRequest) : 0,
    attachmentRequest: attachmentRequest >= 0 && attachmentRequest <= 4294967295 ? Math.trunc(attachmentRequest) : 0,
    placementRequest: placementRequest >= 0 && placementRequest <= 4294967295 ? Math.trunc(placementRequest) : 0,
    attachmentEpoch: readU64(bytes, 50), placementEpoch: readU64(bytes, 58),
  };
}

function readMutation(bytes: Uint8Array, result: CommandResult): CommandResult {
  const mutationOutcome = bytes[66];
  const targetSessionId = readWord(bytes, 68);
  return {
    ...result,
    mutationOutcome: mutationOutcome >= 0 && mutationOutcome <= 255 ? Math.trunc(mutationOutcome) : 0,
    targetSessionId: targetSessionId >= 0 && targetSessionId <= 4294967295 ? Math.trunc(targetSessionId) : 0,
  };
}

function readError(bytes: Uint8Array, result: CommandResult): CommandResult {
  const errorDomain = readWord(bytes, 42);
  const errorCode = readWord(bytes, 46);
  const reason = bytes[5];
  return {
    ...result,
    errorDomain: errorDomain >= 0 && errorDomain <= 4294967295 ? Math.trunc(errorDomain) : 0,
    errorCode: errorCode >= 0 && errorCode <= 4294967295 ? Math.trunc(errorCode) : 0,
    reason: reason >= 0 && reason <= 255 ? Math.trunc(reason) : 0,
  };
}

function decodeResult(bytes: Uint8Array): CommandResult | null {
  if (!validResult(bytes)) return null;
  const id = readU64(bytes, 6);
  if (id.hi === 0 && id.lo === 0) return null;
  const correlation = readCorrelation(bytes, id);
  const outcomes = readOutcomes(bytes, correlation);
  const mutation = readMutation(bytes, outcomes);
  return readError(bytes, mutation);
}

function acknowledge(result: CommandResult): Uint8Array {
  const bytes = new Uint8Array(10);
  bytes[0] = 1;
  bytes[1] = result.source;
  writeU32(bytes, 2, result.id.lo);
  writeU32(bytes, 6, result.id.hi);
  return bytes;
}

function resultNotice(result: CommandResult): Uint8Array {
  if (result.operation === 3) return asciiBytes("Operation outcome unknown. Do not retry blindly.");
  if (result.operation === 2) return asciiBytes("Operation refused.");
  if (result.placement === 4) return asciiBytes("Operation succeeded; placement outcome unknown.");
  if (result.placement === 3) return asciiBytes("Operation succeeded; destination no longer available.");
  if (result.placement === 2) return asciiBytes("Operation succeeded; presentation unavailable.");
  // Successful work stays quiet, including focus superseded by a newer action.
  return EMPTY;
}

function seenResult(recent: readonly CommandResult[], result: CommandResult): boolean {
  for (const previous of recent) {
    if (previous.source === result.source && sameU64(previous.id, result.id)) return true;
  }
  return false;
}

export function receiveCommandResult(state: CommandResults, bytes: Uint8Array): ResultDecision {
  if (bytes.length === 2 && bytes[0] === 1 && bytes[1] === 0) {
    if (state.refreshPending) return requestCommandResults({ ...state, loading: false, deliveryNotice: EMPTY });
    return { state: { ...state, loading: false, deliveryNotice: EMPTY }, request: EMPTY };
  }
  const result = decodeResult(bytes);
  if (result === null) return failedCommandResults(state);
  const acknowledgement = acknowledge(result);
  const duplicate = seenResult(state.recent, result);
  const notice = duplicate ? EMPTY : resultNotice(result);
  return {
    state: {
      ...state, recent: duplicate ? state.recent : [...state.recent.slice(-15), result],
      acknowledgement, loading: true, refreshPending: false, deliveryNotice: EMPTY,
      notice: notice.length === 0 ? state.notice : notice,
    },
    request: acknowledgement,
  };
}

/// A failed read does not establish an operation outcome. The next wake may
/// repeat this acknowledgement/read; it must never repeat the original action.
export function failedCommandResults(state: CommandResults): ResultDecision {
  const next = { ...state, loading: false, deliveryNotice: asciiBytes("Command result unavailable. Work may still be running.") };
  if (state.refreshPending) return requestCommandResults(next);
  return { state: next, request: EMPTY };
}
