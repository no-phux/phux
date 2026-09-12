import { readU64, writeU32 } from "./protocol.ts";

export function windowTarget(target: Uint8Array): boolean {
  if (target.length === 10 && target[0] === 4) return true;
  return target.length === 22 && target[0] === 5;
}

export function windowCommand(id: number, target: Uint8Array): Uint8Array {
  if (!windowTarget(target)) return new Uint8Array(0);
  const request = new Uint8Array(10 + target.length);
  request[0] = 1; request[1] = 1;
  writeU32(request, 2, id);
  for (let at = 0; at < target.length; at += 1) request[10 + at] = target[at];
  return request;
}

export function windowReceipt(body: Uint8Array, id: number): number {
  if (body.length !== 27 || body[0] !== 1) return 0;
  const receipt = readU64(body, 3);
  if (receipt.hi !== 0 || receipt.lo !== id) return 0;
  return body[1] === 1 ? 1 : 2;
}
