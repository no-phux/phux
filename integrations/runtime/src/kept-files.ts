import { randomUUID } from "node:crypto";
import { lstat, mkdir, readdir, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

/**
 * Files a tool hands the agent instead of inlining (an oversized tree, a
 * frame too large to attach) live in one private directory per user. Each
 * new file prunes the rest: none outlives the retention window, and at most
 * MAX_KEPT_FILES exist at once.
 */
export const KEPT_FILE_RETENTION_MS = 10 * 60_000;
export const MAX_KEPT_FILES = 32;
export const KEPT_FILES_NOTE = "Saved files are kept about 10 minutes, at most 32 at a time, in a private temporary directory.";

export function keptFilesDirectory(): string {
  const user = typeof process.getuid === "function" ? String(process.getuid()) : "user";
  return join(tmpdir(), `phux-agent-files-${user}`);
}

/** A fresh path in the private directory, after pruning what retention no longer allows. */
export async function keptFilePath(prefix: string, extension: string, now = Date.now()): Promise<string> {
  const directory = await privateDirectory();
  await prune(directory, now);
  return join(directory, `${prefix}-${now}-${randomUUID().slice(0, 8)}${extension}`);
}

async function privateDirectory(): Promise<string> {
  const directory = keptFilesDirectory();
  await mkdir(directory, { recursive: true, mode: 0o700 });
  // A shared /tmp lets another user pre-create the name; refuse anything but our own private directory.
  const info = await lstat(directory);
  const owner = typeof process.getuid === "function" ? process.getuid() : info.uid;
  if (!info.isDirectory() || info.uid !== owner || (info.mode & 0o077) !== 0) {
    throw new Error(`refusing ${directory}: it must be a directory owned by this user with mode 0700`);
  }
  return directory;
}

async function prune(directory: string, now: number): Promise<void> {
  const entries = await Promise.all((await readdir(directory)).map(async (name) => {
    const path = join(directory, name);
    try {
      return { path, mtime: (await stat(path)).mtimeMs };
    } catch {
      return undefined;
    }
  }));
  const files = entries.filter((entry) => entry !== undefined).sort((a, b) => b.mtime - a.mtime);
  // Leave room for the file about to be written.
  const stale = files.filter((file, index) => index >= MAX_KEPT_FILES - 1 || now - file.mtime > KEPT_FILE_RETENTION_MS);
  await Promise.all(stale.map((file) => rm(file.path, { recursive: true, force: true })));
}
