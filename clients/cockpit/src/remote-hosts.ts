/// Connect to Host: the TypeScript half of Cockpit's remote-host flow.
///
/// The wire is the native half's (src/cockpit/native/remote_hosts.zig):
///   request  version=1, kind, target_len, target
///   reply    version=1, phase, host_len, host, reason_len, reason
/// Hosts are names from the phux CLI's own registry (`phux host add`,
/// `phux host enroll`, `phux --remote`); Cockpit never pairs one itself.
import { asciiBytes } from "@native-sdk/core";

export const REMOTE_KIND_STATUS = 1;
export const REMOTE_KIND_CONNECT = 2;
export const REMOTE_KIND_LOCAL = 3;
/// Remove the remote host entirely: its group leaves the switcher and it is
/// no longer reattached at launch. "Use this Mac" only makes this Mac active.
export const REMOTE_KIND_DISCONNECT = 4;

export const REMOTE_PHASE_LOCAL = 0;
export const REMOTE_PHASE_CONNECTING = 1;
export const REMOTE_PHASE_CONNECTED = 2;
export const REMOTE_PHASE_FAILED = 3;
export const REMOTE_PHASE_RECONNECTING = 4;

export interface RemoteReply {
  readonly phase: number;
  readonly host: Uint8Array;
  readonly reason: Uint8Array;
}

const NONE = new Uint8Array(0);

/// A target longer than one length byte cannot be framed; it is sent empty,
/// which the engine refuses rather than truncating a host name.
export function remoteRequest(kind: number, target: Uint8Array): Uint8Array {
  const length = target.length <= 255 ? target.length : 0;
  const out = new Uint8Array(3 + length);
  out[0] = 1;
  out[1] = kind;
  out[2] = length;
  for (let i = 0; i < length; i += 1) out[3 + i] = target[i];
  return out;
}

export function remoteReply(bytes: Uint8Array): RemoteReply | null {
  if (bytes.length < 4 || bytes[0] !== 1) return null;
  const phase = bytes[1];
  if (!(phase >= REMOTE_PHASE_LOCAL && phase <= REMOTE_PHASE_RECONNECTING)) return null;
  const reasonAt = 3 + bytes[2];
  if (reasonAt >= bytes.length) return null;
  const reasonEnd = reasonAt + 1 + bytes[reasonAt];
  if (reasonEnd !== bytes.length) return null;
  return { phase, host: bytes.subarray(3, reasonAt), reason: bytes.subarray(reasonAt + 1, reasonEnd) };
}

function join(head: Uint8Array, mid: Uint8Array, tail: Uint8Array): Uint8Array {
  const out = new Uint8Array(head.length + mid.length + tail.length);
  let at = 0;
  for (let i = 0; i < head.length; i += 1) {
    out[at] = head[i];
    at += 1;
  }
  for (let i = 0; i < mid.length; i += 1) {
    out[at] = mid[i];
    at += 1;
  }
  for (let i = 0; i < tail.length; i += 1) {
    out[at] = tail[i];
    at += 1;
  }
  return out;
}

/// One status line per phase, naming the host. Empty for this Mac, whose
/// status is the ordinary connection label.
export function remoteStatusLine(reply: RemoteReply): Uint8Array {
  if (reply.phase === REMOTE_PHASE_CONNECTING) return join(asciiBytes("Connecting to "), reply.host, asciiBytes("..."));
  if (reply.phase === REMOTE_PHASE_CONNECTED) return join(asciiBytes("Connected to "), reply.host, NONE);
  if (reply.phase === REMOTE_PHASE_RECONNECTING) return join(asciiBytes("Reconnecting to "), reply.host, asciiBytes("..."));
  if (reply.phase === REMOTE_PHASE_FAILED) {
    return join(join(asciiBytes("Could not connect to "), reply.host, asciiBytes(": ")), reply.reason, NONE);
  }
  return NONE;
}
