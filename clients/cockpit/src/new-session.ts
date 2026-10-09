export interface NewSessionReply {
  readonly phase: number;
  readonly token: Uint8Array;
  readonly host: Uint8Array;
  readonly reason: Uint8Array;
}

export function newSessionRequest(
  kind: number,
  token: Uint8Array,
  name: Uint8Array,
  directory: Uint8Array = new Uint8Array(0),
): Uint8Array {
  if (name.length > 240 || directory.length > 240) return new Uint8Array(0);
  const out = new Uint8Array(11 + name.length);
  out[0] = 1; out[1] = kind;
  if (token.length === 8) for (let at = 0; at < 8; at += 1) out[2 + at] = token[at];
  out[10] = name.length;
  for (let at = 0; at < name.length; at += 1) out[11 + at] = name[at];
  if (directory.length === 0) return out;
  const withDirectory = new Uint8Array(out.length + 1 + directory.length);
  withDirectory.set(out);
  withDirectory[out.length] = directory.length;
  withDirectory.set(directory, out.length + 1);
  return withDirectory;
}

export function newSessionReply(body: Uint8Array): NewSessionReply | null {
  if (body.length < 16 || body[0] !== 1 || body[1] > 4) return null;
  const hostEnd = 15 + body[14];
  if (hostEnd >= body.length) return null;
  if (hostEnd + 1 + body[hostEnd] !== body.length) return null;
  return { phase: body[1], token: body.slice(2, 10), host: body.slice(15, hostEnd), reason: body.slice(hostEnd + 1) };
}
