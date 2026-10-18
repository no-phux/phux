// The two filesystem calls node:fs cannot make exactly, through libc.
//
// `utimesSync` takes seconds as a double, which rounds a 2026 mtime by up to
// a few hundred nanoseconds. Cargo's exported scheduler state records each
// target file's mtime, so the collapse restores it to the nanosecond with
// utimensat(2), as Python's os.utime(ns=...) did.
//
// The APFS clone id has no node:fs equivalent at all; tests read it to prove
// a clonefile copy shares its data stream.
import { endianness } from "node:os";
import { dlopen, FFIType, ptr } from "bun:ffi";

const LIBC = process.platform === "darwin" ? "/usr/lib/libSystem.B.dylib" : "libc.so.6";
const AT_FDCWD = process.platform === "darwin" ? -2 : -100;
const LITTLE = endianness() === "LE";

let library: ReturnType<typeof open> | undefined;

function open() {
  return dlopen(LIBC, {
    utimensat: { args: [FFIType.i32, FFIType.ptr, FFIType.ptr, FFIType.i32], returns: FFIType.i32 },
  });
}

function cString(path: string): Buffer {
  return Buffer.from(`${path}\0`);
}

/** Set atime and mtime to the nanosecond, following symlinks. */
export function utimensNs(path: string, atimeNs: bigint, mtimeNs: bigint): void {
  library ??= open();
  const second = 1_000_000_000n;
  // struct timespec[2] on LP64 Linux and Darwin: two i64 pairs.
  const times = new BigInt64Array([
    atimeNs / second,
    atimeNs % second,
    mtimeNs / second,
    mtimeNs % second,
  ]);
  if (library.symbols.utimensat(AT_FDCWD, cString(path), ptr(times), 0) !== 0) {
    throw new Error(`utimensat failed for ${path}`);
  }
}

// Darwin <sys/attr.h>. Extended common attributes live in forkattr and are
// returned only when FSOPT_ATTR_CMN_EXTENDED is set. Asking for
// ATTR_CMN_RETURNED_ATTRS plus FSOPT_PACK_INVAL_ATTRS puts the clone id at a
// fixed offset: u32 length, then the 20-byte returned attribute set, then the
// u64 id. Reading eight bytes at offset 4 instead returns a per-file value on
// Darwin 27, so identical clones look independent.
const ATTR_BIT_MAP_COUNT = 5;
const ATTR_CMN_RETURNED_ATTRS = 0x8000_0000;
const FSOPT_NOFOLLOW = 0x1;
const FSOPT_PACK_INVAL_ATTRS = 0x8;
const FSOPT_ATTR_CMN_EXTENDED = 0x20;
export const ATTR_CMNEXT_CLONEID = 0x100;
export const CLONE_ID_OFFSET = 24;

/**
 * Parse one clone id from a returned-attribute `getattrlist` buffer.
 *
 * Undefined when the buffer is short, the filesystem did not return
 * ATTR_CMNEXT_CLONEID, or the id is zero. An id parked at offset 4, which is
 * what a length-plus-u64 reading used to accept, is not this layout.
 */
export function cloneIdFromBuffer(raw: Uint8Array): bigint | undefined {
  if (raw.length < CLONE_ID_OFFSET + 8) return undefined;
  const view = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  if (view.getUint32(0, LITTLE) < CLONE_ID_OFFSET + 8) return undefined;
  if ((view.getUint32(20, LITTLE) & ATTR_CMNEXT_CLONEID) === 0) return undefined;
  const id = view.getBigUint64(CLONE_ID_OFFSET, LITTLE);
  return id === 0n ? undefined : id;
}

/**
 * The APFS data-stream id, or undefined when this kernel has none.
 *
 * Zero is not a shared stream. `getattrlist` is Darwin-only; every other
 * platform returns undefined and the caller has to use inode identity.
 */
export function apfsCloneId(path: string): bigint | undefined {
  if (process.platform !== "darwin") return undefined;
  const darwin = dlopen(LIBC, {
    getattrlist: {
      args: [FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.u64, FFIType.u64],
      returns: FFIType.i32,
    },
  });
  try {
    // struct attrlist: u16 bitmapcount, u16 reserved, then five u32 groups.
    const query = new Uint8Array(24);
    const view = new DataView(query.buffer);
    view.setUint16(0, ATTR_BIT_MAP_COUNT, LITTLE);
    view.setUint32(4, ATTR_CMN_RETURNED_ATTRS, LITTLE);
    view.setUint32(20, ATTR_CMNEXT_CLONEID, LITTLE);
    const found = new Uint8Array(64);
    const options = FSOPT_NOFOLLOW | FSOPT_PACK_INVAL_ATTRS | FSOPT_ATTR_CMN_EXTENDED;
    const status = darwin.symbols.getattrlist(
      cString(path),
      ptr(query),
      ptr(found),
      found.length,
      options,
    );
    return status === 0 ? cloneIdFromBuffer(found) : undefined;
  } finally {
    darwin.close();
  }
}
