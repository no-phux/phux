/// Rename Session: the TypeScript half of Cockpit's session rename.
///
/// The wire is the native half's (src/cockpit/native/session_commands.zig):
///   request  version=1, kind, name_len, name
///   reply    version=1, phase, name_len, name, host_len, host, reason_len, reason
/// The session is the one on screen, on the coordinator that owns it; the
/// engine decides which, never the core.

export const SESSION_KIND_DESCRIBE = 1;
export const SESSION_KIND_RENAME = 2;
export const SESSION_KIND_STATUS = 3;

export const SESSION_PHASE_READY = 0;
export const SESSION_PHASE_PENDING = 1;
export const SESSION_PHASE_RENAMED = 2;
export const SESSION_PHASE_REFUSED = 3;
export const SESSION_PHASE_UNAVAILABLE = 4;

export interface SessionReply {
  readonly phase: number;
  readonly name: Uint8Array;
  readonly host: Uint8Array;
  readonly reason: Uint8Array;
}

/// A name longer than one length byte cannot be framed; it is sent empty,
/// which the engine refuses rather than truncating a name.
export function sessionRequest(kind: number, name: Uint8Array): Uint8Array {
  const length = name.length <= 255 ? name.length : 0;
  const out = new Uint8Array(3 + length);
  out[0] = 1;
  out[1] = kind;
  out[2] = length;
  for (let i = 0; i < length; i += 1) out[3 + i] = name[i];
  return out;
}

export function sessionReply(bytes: Uint8Array): SessionReply | null {
  if (bytes.length < 5 || bytes[0] !== 1) return null;
  const phase = bytes[1];
  if (!(phase >= SESSION_PHASE_READY && phase <= SESSION_PHASE_UNAVAILABLE)) return null;
  const hostAt = 3 + bytes[2];
  if (hostAt >= bytes.length) return null;
  const reasonAt = hostAt + 1 + bytes[hostAt];
  if (reasonAt >= bytes.length) return null;
  const end = reasonAt + 1 + bytes[reasonAt];
  if (end !== bytes.length) return null;
  return {
    phase,
    name: bytes.subarray(3, hostAt),
    host: bytes.subarray(hostAt + 1, reasonAt),
    reason: bytes.subarray(reasonAt + 1, end),
  };
}
