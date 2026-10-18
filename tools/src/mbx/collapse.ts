// Point duplicate mbx target files at the matching object-store bytes.
//
// Workspace restore copies on ext4, so a lane holds the object store twice,
// and the export's third copy ran out of disk (run 38505694112). Each target
// file whose bytes match a store blob becomes a hard link to it. Mode and
// mtime live on the inode and Cargo's exported scheduler state records both,
// so a blob is shared only by target files that want the same metadata, and
// that metadata is put back on it to the nanosecond.
import {
  chmodSync,
  closeSync,
  existsSync,
  linkSync,
  openSync,
  readSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  statfsSync,
  type BigIntStats,
} from "node:fs";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import { describe } from "../lib/command.ts";
import { status } from "../lib/output.ts";
import { utimensNs } from "./native.ts";

type Files = Map<string, BigIntStats>;

/** Regular files under `root`; symlinks are neither followed nor listed. */
export function files(root: string, found: Files = new Map()): Files {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) files(path, found);
    else if (entry.isFile()) found.set(path, statSync(path, { bigint: true }));
  }
  return found;
}

const left = Buffer.alloc(1 << 20);
const right = Buffer.alloc(1 << 20);

function withFd<T>(path: string, body: (fd: number) => T): T {
  const fd = openSync(path, "r");
  try {
    return body(fd);
  } finally {
    closeSync(fd);
  }
}

export function sameBytes(a: string, b: string): boolean {
  return withFd(a, (fa) =>
    withFd(b, (fb) => {
      for (;;) {
        const read = readSync(fa, left);
        if (read !== readSync(fb, right)) return false;
        if (read === 0) return true;
        if (!left.subarray(0, read).equals(right.subarray(0, read))) return false;
      }
    }),
  );
}

const inode = (stats: BigIntStats): string => `${stats.dev}:${stats.ino}`;
const metadata = (stats: BigIntStats): string => `${stats.mode & 0o7777n}:${stats.mtimeNs}`;

/** Replace `destination` with a hard link to `source`. */
function link(source: string, destination: string): boolean {
  const temporary = `${destination}.${randomUUID()}`;
  try {
    linkSync(source, temporary);
    renameSync(temporary, destination);
    return true;
  } catch (error) {
    rmSync(temporary, { force: true });
    status("warn", `could not share ${destination}: ${describe(error)}`);
    return false;
  }
}

function freeBytes(path: string): bigint {
  const usage = statfsSync(path, { bigint: true });
  return usage.bavail * usage.bsize;
}

export function collapse(cacheDir: string): void {
  const targetDir = join(cacheDir, "targets");
  const casDir = join(cacheDir, "actions", "cas", "v1");
  if (!existsSync(targetDir) || !existsSync(casDir)) {
    status("info", "no managed target to collapse");
    return;
  }
  const targets = files(targetDir);
  const bySize = Map.groupBy(files(casDir), ([, stats]) => stats.size);
  // The metadata each inode already carries for the target files on it.
  const claimed = new Map([...targets.values()].map((stats) => [inode(stats), metadata(stats)]));
  const before = freeBytes(cacheDir);
  let shared = 0;
  for (const [path, stats] of targets) {
    const want = metadata(stats);
    for (const [source, blob] of bySize.get(stats.size) ?? []) {
      if (inode(blob) === inode(stats)) break;
      if ((claimed.get(inode(blob)) ?? want) !== want || !sameBytes(path, source)) continue;
      if (link(source, path)) {
        chmodSync(path, Number(stats.mode & 0o7777n));
        utimensNs(path, stats.atimeNs, stats.mtimeNs);
        claimed.set(inode(blob), want);
        shared++;
      }
      break;
    }
  }
  const freed = freeBytes(cacheDir) - before;
  status(
    "info",
    `linked ${shared} of ${targets.size} target files to the object store (${freed > 0n ? freed : 0n} bytes freed)`,
  );
}
