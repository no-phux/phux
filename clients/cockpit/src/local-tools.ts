export interface LocalToolReply {
  readonly phase: number;
  readonly operation: number;
  readonly token: Uint8Array;
  readonly target: Uint8Array;
  readonly message: Uint8Array;
}

export function localToolRequest(kind: number, token: Uint8Array, destination: Uint8Array, name: Uint8Array): Uint8Array {
  if (destination.length > 255 || name.length > 255) return new Uint8Array(0);
  const out = new Uint8Array(12 + destination.length + name.length);
  out[0] = 1; out[1] = kind;
  if (token.length === 8) for (let at = 0; at < 8; at += 1) out[2 + at] = token[at];
  out[10] = destination.length;
  for (let at = 0; at < destination.length; at += 1) out[11 + at] = destination[at];
  out[11 + destination.length] = name.length;
  for (let at = 0; at < name.length; at += 1) out[12 + destination.length + at] = name[at];
  return out;
}

export function localToolReply(body: Uint8Array): LocalToolReply | null {
  if (body.length < 18 || body[0] !== 1 || body[1] > 5) return null;
  const targetEnd = 16 + body[14] + body[15] * 256;
  if (targetEnd + 2 > body.length) return null;
  const messageEnd = targetEnd + 2 + body[targetEnd] + body[targetEnd + 1] * 256;
  if (messageEnd !== body.length) return null;
  const rawOperation = body[2] + body[3] * 256 + body[4] * 65536 + body[5] * 16777216;
  const operation = rawOperation >= 0 && rawOperation <= 4294967295 ? Math.trunc(rawOperation) : 0;
  return { phase: body[1], operation, token: body.slice(6, 14), target: body.slice(16, targetEnd), message: body.slice(targetEnd + 2) };
}
