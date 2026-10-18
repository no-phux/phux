// utimensat(2) through libc: node:fs takes seconds as a double, which rounds a
// 2026 mtime by a few hundred nanoseconds, and Cargo's exported scheduler
// state records each target file's mtime exactly.
import { dlopen, FFIType, ptr } from "bun:ffi";

const darwin = process.platform === "darwin";
let libc: ReturnType<typeof open> | undefined;

function open() {
  return dlopen(darwin ? "/usr/lib/libSystem.B.dylib" : "libc.so.6", {
    utimensat: { args: [FFIType.i32, FFIType.ptr, FFIType.ptr, FFIType.i32], returns: FFIType.i32 },
  });
}

/** Set atime and mtime to the nanosecond, following symlinks. */
export function utimensNs(path: string, atimeNs: bigint, mtimeNs: bigint): void {
  libc ??= open();
  const second = 1_000_000_000n;
  // struct timespec[2] on LP64 Linux and Darwin.
  const times = new BigInt64Array([
    atimeNs / second,
    atimeNs % second,
    mtimeNs / second,
    mtimeNs % second,
  ]);
  const atFdcwd = darwin ? -2 : -100;
  if (libc.symbols.utimensat(atFdcwd, Buffer.from(`${path}\0`), ptr(times), 0) !== 0) {
    throw new Error(`utimensat failed for ${path}`);
  }
}
