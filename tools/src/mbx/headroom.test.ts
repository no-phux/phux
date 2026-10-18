// scripts/ci/mbx-export-headroom.sh and the collapse it runs, end to end on
// throwaway trees with fake `mbx`, `df` and `du` on PATH.
import { describe, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { endianness, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { ATTR_CMNEXT_CLONEID, CLONE_ID_OFFSET, apfsCloneId, cloneIdFromBuffer } from "./native.ts";

const SCRIPT = repoPath("scripts/ci/mbx-export-headroom.sh");

interface Identity {
  readonly dev: number;
  readonly ino: number;
}

/**
 * True when the two files are one allocated copy.
 *
 * `hardlink(1)` on ext4 keeps a single inode. APFS clonefile keeps distinct
 * inodes on one data stream, and each clone's st_blocks still reports the
 * full size, so a shared inode is not a portable proof that the duplicate
 * stopped costing disk. A missing or zero clone id is not evidence.
 */
function sharesStoredBytes(
  left: Identity,
  right: Identity,
  leftClone: bigint | undefined,
  rightClone: bigint | undefined,
): boolean {
  if (left.dev !== right.dev) return false;
  if (left.ino === right.ino) return true;
  return Boolean(leftClone) && leftClone === rightClone;
}

function shares(left: string, right: string): boolean {
  return sharesStoredBytes(statSync(left), statSync(right), apfsCloneId(left), apfsCloneId(right));
}

function scratch(body: (root: string) => void): void {
  const root = mkdtempSync(join(tmpdir(), "phux-headroom-"));
  try {
    body(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function write(path: string, content: string | Uint8Array, mode?: number): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content);
  if (mode !== undefined) chmodSync(path, mode);
}

function headroom(env: Record<string, string>): {
  exitCode: number;
  stdout: string;
  stderr: string;
} {
  const result = Bun.spawnSync(["bash", SCRIPT], { env, stdout: "pipe", stderr: "pipe" });
  return {
    exitCode: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

function withoutCi(): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [name, value] of Object.entries(process.env)) {
    if (value !== undefined && name !== "GITHUB_ACTIONS" && name !== "CARGO_HOME")
      env[name] = value;
  }
  return env;
}

describe("export cleanup", () => {
  test("preserves the pending cache and cargo tools", () => {
    // Download cleanup must not evict the current build's export closure.
    scratch((root) => {
      const home = join(root, "home");
      const registry = join(home, ".cargo", "registry", "src");
      const git = join(home, ".cargo", "git", "db");
      const cargoBin = join(home, ".cargo", "bin");
      const keep = join(home, "keep-store");
      for (const path of [registry, git, cargoBin, keep]) write(join(path, "sentinel"), "keep");
      const bin = join(root, "bin");
      // GC after a build invalidates its pending export group. Refuse any mbx
      // call so the filesystem cleanup cannot silently reintroduce it.
      write(join(bin, "mbx"), "#!/bin/sh\nexit 1\n", 0o755);
      const env = {
        ...withoutCi(),
        PATH: `${bin}:${process.env["PATH"] ?? ""}`,
        HOME: home,
        PHUX_MBX_EXPORT_HEADROOM: "1",
      };
      const completed = headroom(env);
      expect(completed.exitCode, completed.stderr).toBe(0);
      expect(existsSync(join(registry, "sentinel"))).toBe(false);
      expect(existsSync(join(git, "sentinel"))).toBe(false);
      expect(existsSync(join(cargoBin, "sentinel"))).toBe(true);
      expect(readFileSync(join(keep, "sentinel"), "utf8")).toBe("keep");

      const skipped = headroom({ ...env, PHUX_MBX_EXPORT_HEADROOM: "0", PATH: "/usr/bin:/bin" });
      expect(skipped.exitCode, skipped.stderr).toBe(0);
      expect(skipped.stdout).toContain("skipped");
    });
  });

  test("refuses an empty Cargo home", () => {
    const refused = headroom({ PATH: "/usr/bin:/bin", PHUX_MBX_EXPORT_HEADROOM: "1", HOME: "" });
    expect(refused.exitCode).not.toBe(0);
    expect(refused.stderr).toContain("refusing");
  });
});

test("the shared-storage signal accepts hardlinks and clone ids", () => {
  // Inode identity is the ext4 proof; a shared clone id is the APFS proof.
  const same = { dev: 7, ino: 1 };
  const other = { dev: 7, ino: 2 };
  const otherDevice = { dev: 8, ino: 1 };
  const shared = new Uint8Array(64);
  const view = new DataView(shared.buffer);
  const host = endianness() === "LE";
  view.setUint32(0, 32, host);
  view.setUint32(20, ATTR_CMNEXT_CLONEID, host);
  view.setBigUint64(CLONE_ID_OFFSET, 9n, host);
  expect(cloneIdFromBuffer(shared)).toBe(9n);
  // Issue #1339 read a packed (length, u64) at offset 4. That window is the
  // returned attribute set, not the stream id, so a short buffer of that
  // shape must not count as a clone id.
  const misplaced = new Uint8Array(12);
  new DataView(misplaced.buffer).setUint32(0, 12, host);
  new DataView(misplaced.buffer).setBigUint64(4, 9n, host);
  expect(cloneIdFromBuffer(misplaced)).toBeUndefined();
  view.setUint32(20, 0, host);
  expect(cloneIdFromBuffer(shared)).toBeUndefined();
  view.setUint32(20, ATTR_CMNEXT_CLONEID, host);
  view.setBigUint64(CLONE_ID_OFFSET, 0n, host);
  expect(cloneIdFromBuffer(shared)).toBeUndefined();

  expect(sharesStoredBytes(same, same, undefined, undefined)).toBe(true);
  expect(sharesStoredBytes(same, other, undefined, undefined)).toBe(false);
  expect(sharesStoredBytes(same, other, 0n, 0n)).toBe(false);
  expect(sharesStoredBytes(same, other, undefined, 9n)).toBe(false);
  expect(sharesStoredBytes(same, other, 4n, 9n)).toBe(false);
  expect(sharesStoredBytes(same, otherDevice, 9n, 9n)).toBe(false);
  expect(sharesStoredBytes(same, other, 9n, 9n)).toBe(true);
});

/**
 * A duplicate target must stop occupying its own copy of the store.
 *
 * ext4 hardlink(1) does that by sharing an inode with the object-store blob.
 * APFS clonefile keeps a distinct inode for the same data stream, so
 * st_blocks still looks like a full copy and inode equality is not the
 * portable signal. Two target files whose mode or mtime disagree still
 * cannot share an inode: the collapse puts that metadata back.
 */
function assertTargetCopiesCollapsed(hideHardlink: boolean): void {
  const payload = Buffer.from("same-object".repeat(1000));
  scratch((root) => {
    const home = join(root, "home");
    const target = join(home, ".cache", "mbx", "targets", "view");
    const cas = join(home, ".cache", "mbx", "actions", "cas", "v1", "blake3", "ab");
    const first = join(target, "libfirst.rlib");
    const second = join(target, "libsecond.rlib");
    const blob = join(cas, "blob");
    const unique = join(target, "only-here");
    write(first, payload, 0o755);
    write(second, payload, 0o640);
    write(blob, payload, 0o444);
    write(unique, "not-in-the-store");
    utimesSync(first, 1_000, 1_000);
    utimesSync(second, 2_000, 2_000);
    utimesSync(blob, 3_000, 3_000);
    const bin = join(root, "bin");
    write(join(bin, "mbx"), "#!/bin/sh\necho mbx-was-called >&2\nexit 1\n", 0o755);
    let path: string;
    if (hideHardlink) {
      for (const tool of ["bash", "df", "du", "rm", "awk", "dirname"]) {
        const source = Bun.which(tool);
        expect(source, tool).not.toBeNull();
        symlinkSync(source ?? "", join(bin, tool));
      }
      symlinkSync(process.execPath, join(bin, "bun"));
      path = bin;
    } else {
      path = `${bin}:${dirname(process.execPath)}:${process.env["PATH"] ?? ""}`;
    }
    const completed = headroom({ PATH: path, HOME: home, PHUX_MBX_EXPORT_HEADROOM: "1" });
    expect(completed.exitCode, completed.stderr).toBe(0);
    expect(completed.stderr).not.toContain("mbx-was-called");
    if (hideHardlink) {
      expect(completed.stderr).toContain("hardlink is not installed");
      expect(completed.stderr).not.toContain("leaving target copies");
    }
    expect(statSync(first).ino).not.toBe(statSync(second).ino);
    const collapsed = shares(first, blob) || shares(second, blob);
    const detail = [first, second, blob]
      .map((file) => {
        const stats = statSync(file);
        return `${stats.ino}/nlink ${stats.nlink}/clone ${apfsCloneId(file)}`;
      })
      .join(", ");
    expect(collapsed, `target copies still occupy their own storage (${detail})`).toBe(true);
    expect(shares(unique, blob)).toBe(false);
    expect(readFileSync(first)).toEqual(payload);
    expect(readFileSync(second)).toEqual(payload);
    expect(readFileSync(blob)).toEqual(payload);
    expect(statSync(first).mode & 0o7777).toBe(0o755);
    expect(Math.trunc(statSync(first).mtimeMs / 1000)).toBe(1_000);
    expect(statSync(second).mode & 0o7777).toBe(0o640);
    expect(Math.trunc(statSync(second).mtimeMs / 1000)).toBe(2_000);
    expect(readFileSync(unique, "utf8")).toBe("not-in-the-store");
    expect(completed.stdout).toContain("collapsed duplicate target files");
  });
}

describe("export headroom collapses target copies", () => {
  test("without calling mbx", () => {
    assertTargetCopiesCollapsed(false);
  });

  test("without a hardlink binary", () => {
    // Darwin's util-linux hardlink is keg-only and often not on PATH. The
    // copies still have to share storage; link(2) is what does that when
    // the binary is missing, and skipping the assertion is not.
    assertTargetCopiesCollapsed(true);
  });
});

test("export headroom skips the save when the copy cannot fit", () => {
  // A store larger than the free disk must not fail the green job. Retiring
  // the pending receipts makes the post step report an empty export. The
  // object bytes stay, and so does a file outside the group.
  scratch((root) => {
    const home = join(root, "home");
    const store = join(home, ".cache", "mbx", "actions");
    const receipt = join(store, "build-receipts", "v1", "groups", "abc", "1.json");
    const kept = join(store, "cas", "v1", "blake3", "ab", "blob");
    write(receipt, '{"group":"keep-until-the-disk-says-no"}');
    write(kept, "object");
    const bin = join(root, "bin");
    write(
      join(bin, "df"),
      [
        "#!/bin/sh",
        'if [ "$1" = "-h" ]; then',
        "  echo 'Filesystem Size Used Avail Use% Mounted on'",
        "  echo '/dev/root 145G 145G 1M 100% /'",
        "  exit 0",
        "fi",
        "echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'",
        "echo '/dev/root 100000000 99999000 1000 100% /'",
        "",
      ].join("\n"),
      0o755,
    );
    write(join(bin, "du"), "#!/bin/sh\necho '8000000\t'$2\n", 0o755);
    const completed = headroom({
      PATH: `${bin}:/usr/bin:/bin`,
      HOME: home,
      PHUX_MBX_EXPORT_HEADROOM: "1",
    });
    expect(completed.exitCode, completed.stderr).toBe(0);
    expect(completed.stderr).toContain("not saving this store");
    expect(existsSync(receipt)).toBe(false);
    expect(readFileSync(kept, "utf8")).toBe("object");

    write(receipt, "still-pending");
    const keptRun = headroom({ PATH: "/usr/bin:/bin", HOME: home, PHUX_MBX_EXPORT_HEADROOM: "1" });
    expect(keptRun.exitCode, keptRun.stderr).toBe(0);
    expect(keptRun.stderr).not.toContain("not saving this store");
    expect(readFileSync(receipt, "utf8")).toBe("still-pending");
  });
});
