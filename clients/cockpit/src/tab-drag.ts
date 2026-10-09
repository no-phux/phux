import { writeU32 } from "./protocol.ts";

/// Host payload for `cockpit.tab-drag`: source tab id, phase, and the pointer
/// in whole view points. The engine hit-tests the shipping tab frames.
export function encodeTabDrag(sourceId: number, phase: number, x: number, y: number): Uint8Array {
  const bytes = new Uint8Array(10);
  writeU32(bytes, 0, sourceId >= 1 && sourceId <= 4294967295 ? sourceId : 0);
  bytes[4] = phase === 0 || phase === 1 || phase === 2 ? phase : 255;
  writeI16(bytes, 6, pointCoordinate(x));
  writeI16(bytes, 8, pointCoordinate(y));
  return bytes;
}

function pointCoordinate(value: number): number {
  if (!(value >= -32768 && value <= 32767)) return 0;
  const nudged = value >= 0 ? value + 0.5 : value - 0.5;
  const whole = Math.trunc(nudged);
  if (!(whole >= -32768 && whole <= 32767)) return 0;
  return whole;
}

function writeI16(bytes: Uint8Array, at: number, value: number): void {
  const raw = value < 0 ? value + 65536 : value;
  bytes[at] = raw % 256;
  bytes[at + 1] = Math.floor(raw / 256) % 256;
}
