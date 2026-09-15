import { asciiBytes } from "@native-sdk/core";

export interface SelfUpdate {
  readonly status: number;
  readonly canInstall: boolean;
  readonly relaunch: boolean;
  readonly current: Uint8Array;
  readonly latest: Uint8Array;
  readonly message: Uint8Array;
  readonly remedy: Uint8Array;
}

const EMPTY = new Uint8Array(0);

export function initialSelfUpdate(): SelfUpdate {
  return { status: 255, canInstall: false, relaunch: false, current: EMPTY, latest: EMPTY,
    message: asciiBytes("Check the latest Phux Cockpit release without leaving the app."), remedy: EMPTY };
}

export function selfUpdateRequest(install: boolean): Uint8Array {
  return new Uint8Array([1, install ? 1 : 0]);
}

function readField(bytes: Uint8Array, at: number): { field: Uint8Array, next: number } | null {
  if (at + 2 > bytes.length) return null;
  const length = bytes[at] + bytes[at + 1] * 256;
  const start = at + 2;
  if (start + length > bytes.length) return null;
  return { field: bytes.slice(start, start + length), next: start + length };
}

export function selfUpdateResponse(bytes: Uint8Array): SelfUpdate | null {
  if (bytes.length < 5 || bytes[0] !== 1 || bytes[1] > 4 || bytes[2] > 3) return null;
  const current = readField(bytes, 3);
  if (current === null) return null;
  const latest = readField(bytes, current.next);
  if (latest === null) return null;
  const message = readField(bytes, latest.next);
  if (message === null) return null;
  const remedy = readField(bytes, message.next);
  if (remedy === null || remedy.next !== bytes.length) return null;
  const status = bytes[1];
  return {
    status: status >= 0 && status <= 4 ? Math.trunc(status) : 3,
    canInstall: (bytes[2] & 1) === 1,
    relaunch: (bytes[2] & 2) === 2,
    current: current.field, latest: latest.field, message: message.field, remedy: remedy.field,
  };
}
