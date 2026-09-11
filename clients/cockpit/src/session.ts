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
/// The Empty session state: New Tab in the empty session a window shows,
/// and dismissing a picked one. Neither carries a name.
export const SESSION_KIND_NEW_TAB = 4;
export const SESSION_KIND_DISMISS = 5;
/// Describe and rename for the session a switcher row names: the request
/// carries the row's captured target after the name, and the engine resolves
/// it against the coordinator that listed the row, never another.
export const SESSION_KIND_DESCRIBE_ROW = 6;
export const SESSION_KIND_RENAME_ROW = 7;

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

/// A row kind's request: `sessionRequest` followed by target_len and the
/// row's opaque target. A target that cannot be framed is sent empty, which
/// the engine refuses; nothing is ever sent without the row's own target.
export function sessionRowRequest(kind: number, name: Uint8Array, target: Uint8Array): Uint8Array {
  const head = sessionRequest(kind, name);
  const length = target.length <= 255 ? target.length : 0;
  const out = new Uint8Array(head.length + 1 + length);
  for (let i = 0; i < head.length; i += 1) out[i] = head[i];
  out[head.length] = length;
  for (let i = 0; i < length; i += 1) out[head.length + 1 + i] = target[i];
  return out;
}

/// Whether a switcher row's captured target names a session, on the active
/// coordinator (resource 2) or on a peer (resource 3), rather than a terminal
/// or a peer's unavailable row (session 0). Only such a row offers Rename.
export function sessionRowTarget(target: Uint8Array): boolean {
  if (target.length !== 38 || target[0] !== 2) return false;
  if (target[1] !== 2 && target[1] !== 3) return false;
  return target[34] !== 0 || target[35] !== 0 || target[36] !== 0 || target[37] !== 0;
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
