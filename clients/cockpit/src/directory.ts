/// Go to Directory: the TypeScript half of Cockpit's directory picker.
///
/// The wire is the native half's (src/cockpit/native/directory_picker.zig,
/// docs/DIRECTORY_PICKER.md). All integers are little-endian.
///   request  version=1, kind, request_id[4], offset:u16, index:u16,
///            query_len:u8, query
///   reply    version=1, status, request_id[4], flags, error, total:u16,
///            offset:u16, path_len, path, query_len, query, count,
///            rows (index:u16, flags, name_len, name), message_len, message
/// The request ID is carried as four opaque bytes: the core only ever echoes
/// it and compares it, so it never needs an integer proof.
import { asciiBytes } from "@native-sdk/core";

export const DIR_KIND_OPEN = 1;
export const DIR_KIND_PAGE = 2;
export const DIR_KIND_DESCEND = 3;
export const DIR_KIND_PARENT = 4;
export const DIR_KIND_HERE = 5;

export const DIR_STATUS_UNSUPPORTED = 0;
export const DIR_STATUS_PENDING = 1;
export const DIR_STATUS_LISTED = 2;
export const DIR_STATUS_REFUSED = 3;
export const DIR_STATUS_UNKNOWN = 4;
export const DIR_STATUS_UNAVAILABLE = 5;

/// Synthetic rows the engine lists ahead of the entries while the filter is
/// empty: open a tab in the listed directory, and go up to its parent.
export const DIR_HERE = 65535;
export const DIR_UP = 65534;

/// Whose directories a page names (the reply trailer's scope byte).
export const DIR_SCOPE_COORDINATOR = 0;
/// A satellite of the connected hub, relayed by it (L3 section 4.1).
export const DIR_SCOPE_SATELLITE = 1;
/// The coordinator's own host, in place of the focused satellite's, because
/// that hub cannot list a satellite.
export const DIR_SCOPE_INSTEAD = 2;

export interface DirectoryRow {
  readonly index: number;
  readonly symlink: boolean;
  readonly name: Uint8Array;
}

export interface DirectoryPage {
  readonly status: number;
  readonly request: Uint8Array;
  readonly truncated: boolean;
  readonly total: number;
  readonly offset: number;
  readonly path: Uint8Array;
  readonly query: Uint8Array;
  readonly rows: readonly DirectoryRow[];
  readonly message: Uint8Array;
  readonly scope: number;
  readonly host: Uint8Array;
}

export const NO_DIRECTORY_REQUEST = new Uint8Array(4);

function u16(value: number): number {
  return value >= 0 && value <= 65535 ? Math.trunc(value) : 0;
}

export function directoryRequest(kind: number, request: Uint8Array, offset: number, index: number, query: Uint8Array): Uint8Array {
  const length = query.length <= 64 ? query.length : 0;
  const out = new Uint8Array(11 + length);
  out[0] = 1;
  out[1] = kind;
  for (let i = 0; i < 4; i += 1) out[2 + i] = request.length === 4 ? request[i] : 0;
  const at = u16(offset);
  out[6] = at % 256;
  out[7] = Math.floor(at / 256);
  const row = u16(index);
  out[8] = row % 256;
  out[9] = Math.floor(row / 256);
  out[10] = length;
  for (let i = 0; i < length; i += 1) out[11 + i] = query[i];
  return out;
}

const NO_ROWS: readonly DirectoryRow[] = [];

/// Rows, bounded by the reply: a malformed record rejects the whole page.
function readRows(bytes: Uint8Array, start: number, count: number): { rows: readonly DirectoryRow[]; end: number } | null {
  const rows: DirectoryRow[] = [];
  let at = start;
  for (let i = 0; i < count; i += 1) {
    if (at + 4 > bytes.length) return null;
    const nameEnd = at + 4 + bytes[at + 3];
    if (nameEnd > bytes.length) return null;
    rows.push({ index: bytes[at] + bytes[at + 1] * 256, symlink: (bytes[at + 2] & 1) === 1, name: bytes.subarray(at + 4, nameEnd) });
    at = nameEnd;
  }
  return { rows: rows.length === 0 ? NO_ROWS : rows, end: at };
}

export function directoryPage(bytes: Uint8Array): DirectoryPage | null {
  if (bytes.length < 16 || bytes[0] !== 1 || bytes[1] > DIR_STATUS_UNAVAILABLE) return null;
  const pathEnd = 13 + bytes[12];
  if (pathEnd >= bytes.length) return null;
  const queryEnd = pathEnd + 1 + bytes[pathEnd];
  if (queryEnd >= bytes.length) return null;
  const listed = readRows(bytes, queryEnd + 1, bytes[queryEnd]);
  if (listed === null || listed.end >= bytes.length) return null;
  const messageEnd = listed.end + 1 + bytes[listed.end];
  if (messageEnd + 2 > bytes.length || bytes[messageEnd] > DIR_SCOPE_INSTEAD) return null;
  const hostEnd = messageEnd + 2 + bytes[messageEnd + 1];
  if (hostEnd !== bytes.length) return null;
  return {
    status: bytes[1],
    request: bytes.slice(2, 6),
    truncated: (bytes[6] & 1) === 1,
    total: bytes[8] + bytes[9] * 256,
    offset: bytes[10] + bytes[11] * 256,
    path: bytes.subarray(13, pathEnd),
    query: bytes.subarray(pathEnd + 1, queryEnd),
    rows: listed.rows,
    message: bytes.subarray(listed.end + 1, messageEnd),
    scope: bytes[messageEnd],
    host: bytes.subarray(messageEnd + 2, hostEnd),
  };
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

/// What a row says: the synthetic rows by role, an entry by its name, with a
/// trailing slash so directories read as directories and a marker for links.
export function directoryRowLabel(row: DirectoryRow): Uint8Array {
  if (row.index === DIR_HERE) return asciiBytes("Open a new tab here");
  if (row.index === DIR_UP) return asciiBytes("..");
  return join(row.name, asciiBytes("/"), row.symlink ? asciiBytes("  (link)") : new Uint8Array(0));
}

/// The picker's heading names the host whenever it is not simply the
/// connected coordinator's: a satellite by name, and a hub that cannot list
/// the focused satellite by saying whose directories are shown instead.
export function directoryTitle(page: DirectoryPage): Uint8Array {
  if (page.host.length === 0) return asciiBytes("Go to Directory");
  if (page.scope === DIR_SCOPE_SATELLITE) return join(asciiBytes("Go to Directory on "), page.host, new Uint8Array(0));
  if (page.scope === DIR_SCOPE_INSTEAD) return join(asciiBytes("Go to Directory on the coordinator, not "), page.host, new Uint8Array(0));
  return asciiBytes("Go to Directory");
}

/// The notice under the list for one settled or waiting page.
export function directoryNotice(page: DirectoryPage): Uint8Array {
  if (page.status === DIR_STATUS_PENDING) return join(asciiBytes("Listing "), page.path.length > 0 ? page.path : asciiBytes("home"), asciiBytes("..."));
  if (page.status === DIR_STATUS_REFUSED) return join(join(asciiBytes("Could not list "), page.path, asciiBytes(": ")), page.message, new Uint8Array(0));
  if (page.status === DIR_STATUS_UNKNOWN) return asciiBytes("The connection ended before the listing arrived. Try again.");
  if (page.status !== DIR_STATUS_LISTED) return page.message.length > 0 ? page.message : asciiBytes("Directory listing unavailable. Try again.");
  if (page.total === 0) return asciiBytes("No matching directories");
  if (page.truncated) return asciiBytes("Enter to open  /  Showing the first 1024 directories");
  return asciiBytes("Enter to open  /  Escape to cancel");
}
