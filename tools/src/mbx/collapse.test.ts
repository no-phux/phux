import { expect, test } from "bun:test";
import {
  linkSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";
import { iterFiles, replaceWithShared, restore, sameBytes, snapshot } from "./collapse.ts";
import { utimensNs } from "./native.ts";

function scratch(body: (root: string) => void): void {
  const root = mkdtempSync(join(tmpdir(), "phux-collapse-"));
  try {
    body(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("the walk yields regular files and never follows symlinks", () => {
  scratch((root) => {
    mkdirSync(join(root, "a", "deep"), { recursive: true });
    mkdirSync(join(root, "elsewhere"));
    writeFileSync(join(root, "a", "file"), "x");
    writeFileSync(join(root, "a", "deep", "nested"), "y");
    writeFileSync(join(root, "elsewhere", "outside"), "z");
    symlinkSync("file", join(root, "a", "link"));
    symlinkSync("../elsewhere", join(root, "a", "dirlink"));
    symlinkSync("missing", join(root, "a", "dangling"));
    const found = [...iterFiles(join(root, "a"))].map((path) => relative(root, path)).sort();
    expect(found).toEqual(["a/deep/nested", "a/file"]);
    expect([...iterFiles(join(root, "absent"))]).toEqual([]);
  });
});

test("restore puts mode and nanosecond mtime back and splits conflicting names", () => {
  scratch((root) => {
    const store = join(root, "blob");
    const first = join(root, "first");
    const second = join(root, "second");
    for (const [path, mode] of [
      [store, 0o444],
      [first, 0o755],
      [second, 0o640],
    ] as const) {
      writeFileSync(path, "same", { mode });
    }
    utimensNs(first, 1_000_000_000_123n, 1_760_000_000_123_456_789n);
    utimensNs(second, 2_000_000_000_456n, 1_760_000_001_987_654_321n);
    const saved = snapshot(root);
    saved.delete(store);
    expect(replaceWithShared(store, first)).toBe(true);
    expect(replaceWithShared(store, second)).toBe(true);
    expect(statSync(first).ino).toBe(statSync(store).ino);
    expect(statSync(second).ino).toBe(statSync(store).ino);

    restore(saved);
    const one = statSync(first, { bigint: true });
    const two = statSync(second, { bigint: true });
    expect(Number(one.mode) & 0o7777).toBe(0o755);
    expect(one.mtimeNs).toBe(1_760_000_000_123_456_789n);
    expect(Number(two.mode) & 0o7777).toBe(0o640);
    expect(two.mtimeNs).toBe(1_760_000_001_987_654_321n);
    // The name restored first keeps the shared inode; the other needed
    // different metadata, so it was copied apart.
    const shared = statSync(store, { bigint: true }).ino;
    expect([one.ino, two.ino].filter((ino) => ino === shared)).toHaveLength(1);
  });
});

test("names that want the same metadata stay on one inode", () => {
  scratch((root) => {
    const first = join(root, "first");
    const second = join(root, "second");
    writeFileSync(first, "same", { mode: 0o600 });
    linkSync(first, second);
    utimensNs(first, 1n, 5_000_000_007n);
    const saved = snapshot(root);
    restore(saved);
    expect(statSync(first).ino).toBe(statSync(second).ino);
  });
});

test("byte comparison reads to the end", () => {
  scratch((root) => {
    const size = (1 << 20) + 3;
    const left = Buffer.alloc(size, 1);
    const right = Buffer.alloc(size, 1);
    right[size - 1] = 2;
    writeFileSync(join(root, "left"), left);
    writeFileSync(join(root, "same"), left);
    writeFileSync(join(root, "right"), right);
    writeFileSync(join(root, "short"), left.subarray(0, size - 1));
    expect(sameBytes(join(root, "left"), join(root, "same"))).toBe(true);
    expect(sameBytes(join(root, "left"), join(root, "right"))).toBe(false);
    expect(sameBytes(join(root, "left"), join(root, "short"))).toBe(false);
  });
});
