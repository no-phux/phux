import { asciiBytes } from "@native-sdk/core";

export interface Appearance {
  readonly active: boolean;
  readonly dirty: boolean;
  readonly outcome: number;
  readonly theme: number;
  readonly cursor: number;
  readonly placement: number;
  readonly overrides: boolean;
  readonly fontLabel: Uint8Array;
  readonly contrastLabel: Uint8Array;
  readonly notice: Uint8Array;
  readonly values: readonly Uint8Array[];
  readonly followSystem: boolean;
}

export function initialAppearance(): Appearance {
  return { active: false, dirty: false, outcome: 0, theme: 255, cursor: 0, placement: 0,
    overrides: false, fontLabel: asciiBytes("Loading..."), contrastLabel: new Uint8Array(0), notice: new Uint8Array(0),
    values: [], followSystem: false };
}

export function appearanceRequest(action: number, argument: number): Uint8Array {
  return new Uint8Array([1, action, argument]);
}

function outcomeNotice(outcome: number): Uint8Array {
  if (outcome === 3) return asciiBytes("Could not save or reload. Your current settings are retained. Check the configuration path and permissions, then retry or cancel.");
  if (outcome === 4) return asciiBytes("No configuration destination. Cancel to restore your appearance.");
  if (outcome === 5) return asciiBytes("This change could not be applied. Try again.");
  if (outcome === 6) return asciiBytes("Configuration changed outside Settings. Cancel your preview, then Reload Configuration and try again.");
  if (outcome === 7) return asciiBytes("Configuration contains an invalid value or line. Your last-good settings are retained. Repair the file and reload.");
  return new Uint8Array(0);
}

function validFlags(bytes: Uint8Array): boolean {
  if (bytes[1] > 1 || bytes[3] > 1 || bytes[7] > 1) return false;
  return bytes[2] <= 7 && bytes[5] <= 2 && bytes[6] <= 1;
}

function validResponse(bytes: Uint8Array): boolean {
  if (bytes.length < 10) return false;
  if (bytes[0] !== 1 && bytes[0] !== 2) return false;
  if (!validFlags(bytes)) return false;
  const end = 10 + bytes[8] + bytes[9];
  if (bytes[0] === 1) return end === bytes.length;
  return end < bytes.length;
}

function readValues(bytes: Uint8Array, start: number): readonly Uint8Array[] | null {
  const values: Uint8Array[] = [];
  let at = start;
  while (at < bytes.length) {
    if (at + 3 > bytes.length) return null;
    const id = bytes[at];
    const length = bytes[at + 1] + bytes[at + 2] * 256;
    if (id !== values.length || id > 10) return null;
    at += 3;
    if (at + length > bytes.length) return null;
    values.push(bytes.slice(at, at + length));
    at += length;
  }
  return values.length === 11 ? values : null;
}

// The AOT boundary needs local wholeness proofs, even for validated byte input.
function decodedAppearance(bytes: Uint8Array, fontEnd: number, contrastEnd: number,
  values: readonly Uint8Array[], followSystem: boolean): Appearance {
  const theme = bytes[4];
  const cursor = bytes[5];
  const placement = bytes[6];
  const outcome = bytes[2];
  return {
    theme: theme >= 0 && theme <= 255 ? Math.trunc(theme) : 255,
    cursor: cursor >= 0 && cursor <= 2 ? Math.trunc(cursor) : 0,
    placement: placement >= 0 && placement <= 1 ? Math.trunc(placement) : 0,
    outcome: outcome >= 0 && outcome <= 7 ? Math.trunc(outcome) : 5,
    active: bytes[1] === 1, dirty: bytes[3] === 1, overrides: bytes[7] === 1,
    fontLabel: bytes.slice(10, fontEnd), contrastLabel: bytes.slice(fontEnd, contrastEnd), notice: outcomeNotice(bytes[2]),
    values, followSystem,
  };
}

export function appearanceResponse(bytes: Uint8Array): Appearance | null {
  if (!validResponse(bytes)) return null;
  const fontEnd = 10 + bytes[8];
  const contrastEnd = fontEnd + bytes[9];
  if (bytes[0] === 2 && bytes[contrastEnd] > 1) return null;
  const values = bytes[0] === 2 ? readValues(bytes, contrastEnd + 1) : [];
  if (values === null) return null;
  const followSystem = bytes[0] === 2 && bytes[contrastEnd] === 1;
  return decodedAppearance(bytes, fontEnd, contrastEnd, values, followSystem);
}
