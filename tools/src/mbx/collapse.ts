// Collapse duplicate mbx target bytes onto the object store.
//
// Workspace restore reflinks when the filesystem can, and copies otherwise.
// GitHub-hosted ext4 copies, so the test lane holds the object store twice:
// once under `actions/cas` and again in the managed target. The post step
// then byte-copies the store into the export directory. Run 38505694112 had
// a 36.5 GiB store, a 37.3 GiB target, and 30 GiB free, and that copy
// returned ENOSPC after the tests had passed.
//
// `hardlink(1)` keeps one inode for identical contents. It is often absent
// on Darwin (util-linux is keg-only), so identical bytes are also linked
// with link(2), then clonefile when a hard link is refused. Cargo's exported
// scheduler state records each target file's mode and mtime, and a hard link
// keeps whichever inode it chooses, so this restores that metadata
// afterwards. Two target files that need different metadata cannot share an
// inode; the second one is copied back apart. A clone keeps its own
// metadata, so both names can stay shared.
import {
  chmodSync,
  closeSync,
  constants,
  copyFileSync,
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
import { randomBytes } from "node:crypto";
import { dirname, join } from "node:path";
import { utimensNs } from "./native.ts";

const TAG = "mbx-export-headroom";

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isMissing(error: unknown): boolean {
  return error instanceof Error && "code" in error && error.code === "ENOENT";
}

function statOrUndefined(path: string): BigIntStats | undefined {
  try {
    return statSync(path, { bigint: true });
  } catch (error) {
    if (isMissing(error)) return undefined;
    throw error;
  }
}

/**
 * Regular files under `root`, top-down like os.walk: a directory's own files
 * first, then each subdirectory in listing order. Symlinks are neither
 * followed nor yielded; an unreadable directory is skipped.
 */
export function* iterFiles(root: string): Generator<string> {
  let entries;
  try {
    entries = readdirSync(root, { withFileTypes: true });
  } catch {
    return;
  }
  const directories: string[] = [];
  for (const entry of entries) {
    if (entry.isDirectory()) directories.push(entry.name);
    else if (!entry.isSymbolicLink()) yield join(root, entry.name);
  }
  for (const directory of directories) yield* iterFiles(join(root, directory));
}

export type Snapshot = Map<string, BigIntStats>;

export function snapshot(root: string): Snapshot {
  const found: Snapshot = new Map();
  for (const path of iterFiles(root)) {
    const stats = statOrUndefined(path);
    if (stats !== undefined) found.set(path, stats);
  }
  return found;
}

/** A name beside `path` that did not exist, created and closed (mkstemp). */
function temporaryBeside(path: string, prefix: string): string {
  for (;;) {
    const candidate = join(dirname(path), `${prefix}${randomBytes(6).toString("hex")}`);
    try {
      closeSync(openSync(candidate, "wx", 0o600));
      return candidate;
    } catch (error) {
      if (!(error instanceof Error && "code" in error && error.code === "EEXIST")) throw error;
    }
  }
}

/** Give `path` its own inode with the same bytes. */
function splitCopy(path: string): void {
  const temporary = temporaryBeside(path, ".phux-split-");
  try {
    copyFileSync(path, temporary);
    renameSync(temporary, path);
  } catch (error) {
    rmSync(temporary, { force: true });
    throw error;
  }
}

function inodeKey(stats: BigIntStats): string {
  return `${stats.dev}:${stats.ino}`;
}

/** Put each target file's mode and mtime back after hardlink. */
export function restore(saved: Snapshot): void {
  const applied = new Map<string, string>();
  for (const [path, old] of saved) {
    let current = statOrUndefined(path);
    if (current === undefined) continue;
    const mode = Number(old.mode) & 0o7777;
    const want = `${mode}:${old.mtimeNs}`;
    const previous = applied.get(inodeKey(current));
    if (previous !== undefined && previous !== want) {
      try {
        splitCopy(path);
      } catch (error) {
        console.error(`${TAG}: could not split ${path}: ${errorText(error)}`);
        continue;
      }
      current = statSync(path, { bigint: true });
    }
    chmodSync(path, mode);
    utimensNs(path, old.atimeNs, old.mtimeNs);
    applied.set(inodeKey(current), want);
  }
}

function freeBytes(path: string): bigint {
  const usage = statfsSync(path, { bigint: true });
  return usage.bavail * usage.bsize;
}

function sameInode(left: string, right: string): boolean {
  const leftStats = statOrUndefined(left);
  const rightStats = statOrUndefined(right);
  if (leftStats === undefined || rightStats === undefined) return false;
  return leftStats.dev === rightStats.dev && leftStats.ino === rightStats.ino;
}

/** Byte equality of two files, read in chunks. */
export function sameBytes(left: string, right: string): boolean {
  const size = 1 << 20;
  const leftFd = openSync(left, "r");
  try {
    const rightFd = openSync(right, "r");
    try {
      const leftBuffer = Buffer.alloc(size);
      const rightBuffer = Buffer.alloc(size);
      for (;;) {
        const leftRead = readSync(leftFd, leftBuffer, 0, size, null);
        const rightRead = readSync(rightFd, rightBuffer, 0, size, null);
        if (leftRead !== rightRead) return false;
        if (leftRead === 0) return true;
        if (!leftBuffer.subarray(0, leftRead).equals(rightBuffer.subarray(0, rightRead))) {
          return false;
        }
      }
    } finally {
      closeSync(rightFd);
    }
  } finally {
    closeSync(leftFd);
  }
}

/** Clone `source` onto a path that must not exist yet. Darwin only. */
function cloneApfs(source: string, destination: string): boolean {
  if (process.platform !== "darwin") return false;
  try {
    // FICLONE_FORCE fails rather than falling back to a byte copy.
    copyFileSync(source, destination, constants.COPYFILE_FICLONE_FORCE);
    return true;
  } catch {
    return false;
  }
}

/** Replace `destination` with a hard link to `source`, or an APFS clone. */
export function replaceWithShared(source: string, destination: string): boolean {
  const temporary = temporaryBeside(destination, ".phux-share-");
  rmSync(temporary);
  try {
    try {
      linkSync(source, temporary);
    } catch {
      if (!cloneApfs(source, temporary)) return false;
    }
    renameSync(temporary, destination);
  } catch (error) {
    rmSync(temporary, { force: true });
    console.error(`${TAG}: could not share ${destination}: ${errorText(error)}`);
    return false;
  }
  return true;
}

/**
 * Point each duplicate target file at the matching object-store bytes.
 *
 * `hardlink(1)` is the fast path when it is installed and actually links.
 * On Darwin that binary is frequently missing, and a no-op there used to
 * leave every target on its own inode. link(2) is the same kernel operation.
 * clonefile covers a volume that refuses new hard links; clones stay
 * distinct inodes, which `restore` can chmod independently.
 */
export function shareTargetBytes(targets: string, cas: string): void {
  const bySize = new Map<bigint, string[]>();
  for (const path of iterFiles(cas)) {
    const stats = statOrUndefined(path);
    if (stats === undefined) continue;
    const bucket = bySize.get(stats.size) ?? [];
    bucket.push(path);
    bySize.set(stats.size, bucket);
  }
  for (const destination of iterFiles(targets)) {
    const stats = statOrUndefined(destination);
    if (stats === undefined) continue;
    for (const source of bySize.get(stats.size) ?? []) {
      if (sameInode(destination, source)) break;
      let identical: boolean;
      try {
        identical = sameBytes(destination, source);
      } catch {
        continue;
      }
      if (!identical) continue;
      replaceWithShared(source, destination);
      break;
    }
  }
}

function runHardlink(targets: string, cas: string): void {
  const hardlink = Bun.which("hardlink");
  if (hardlink === null) {
    console.error(`${TAG}: hardlink is not installed; linking copies directly`);
    return;
  }
  const command = [hardlink, "-c", "-q"];
  // Darwin's util-linux build has no reflink option and uses hardlinks.
  if (process.platform !== "darwin") command.push("--reflink=never");
  command.push(targets, cas);
  const completed = Bun.spawnSync(command, { stdout: "pipe", stderr: "pipe" });
  if (completed.exitCode !== 0) {
    process.stderr.write(completed.stderr);
    console.error(`${TAG}: hardlink failed; linking remaining copies directly`);
  }
}

export function collapse(cacheDir: string): void {
  const targets = join(cacheDir, "targets");
  const cas = join(cacheDir, "actions", "cas", "v1");
  if (!isDirectory(targets) || !isDirectory(cas)) {
    console.log(`${TAG}: no managed target to collapse`);
    return;
  }
  const saved = snapshot(targets);
  if (saved.size === 0) {
    console.log(`${TAG}: managed target is empty`);
    return;
  }
  const before = freeBytes(cacheDir);
  runHardlink(targets, cas);
  shareTargetBytes(targets, cas);
  restore(saved);
  const freed = freeBytes(cacheDir) - before;
  console.log(
    `${TAG}: collapsed duplicate target files onto the object store ` +
      `(${saved.size} files, ${freed > 0n ? freed : 0n} bytes freed)`,
  );
}

function isDirectory(path: string): boolean {
  return existsSync(path) && statSync(path).isDirectory();
}
