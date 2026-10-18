// scripts/ci/mbx-export-headroom.sh and the collapse it runs, on throwaway trees.
import { expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { collapse, sameBytes } from "./collapse.ts";
import { utimensNs } from "./native.ts";

function scratch(body: (root: string) => void): void {
  const root = mkdtempSync(join(tmpdir(), "phux-headroom-"));
  try {
    body(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function put(path: string, content: string | Uint8Array, mode = 0o644, mtimeNs?: bigint): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content, { mode });
  if (mtimeNs !== undefined) utimensNs(path, mtimeNs, mtimeNs);
}

function headroom(env: Record<string, string>) {
  const result = Bun.spawnSync(["bash", repoPath("scripts/ci/mbx-export-headroom.sh")], { env });
  return {
    code: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

test("duplicates link to the store blob only when their metadata agrees", () => {
  scratch((root) => {
    const payload = "same-object".repeat(1000);
    const blob = join(root, "actions/cas/v1/blake3/ab/blob");
    const files: [string, number, bigint][] = [
      ["first", 0o755, 1_760_000_000_123_456_789n],
      ["deep/third", 0o755, 1_760_000_000_123_456_789n],
      ["second", 0o640, 1_760_000_001_987_654_321n],
    ];
    put(blob, payload, 0o444);
    for (const [name, mode, mtime] of files) put(join(root, "targets", name), payload, mode, mtime);
    put(join(root, "targets/unique"), "not-in-the-store");
    put(join(root, "outside/copy"), payload);
    symlinkSync("../outside", join(root, "targets/dirlink"));
    symlinkSync("missing", join(root, "targets/dangling"));
    const outside = statSync(join(root, "outside/copy")).ino;

    collapse(root);

    const blobIno = statSync(blob).ino;
    const sharing = files.filter(([name]) => statSync(join(root, "targets", name)).ino === blobIno);
    expect(sharing.length).toBeGreaterThan(0);
    expect(new Set(sharing.map(([, mode, mtime]) => `${mode}:${mtime}`)).size).toBe(1);
    for (const [name, mode, mtime] of files) {
      const path = join(root, "targets", name);
      const stats = statSync(path, { bigint: true });
      expect([name, Number(stats.mode & 0o7777n), stats.mtimeNs]).toEqual([name, mode, mtime]);
      expect(readFileSync(path, "utf8")).toBe(payload);
    }
    expect(statSync(join(root, "targets/unique")).ino).not.toBe(blobIno);
    expect(statSync(join(root, "outside/copy")).ino).toBe(outside);
  });
});

test("byte comparison reads past the first chunk", () => {
  scratch((root) => {
    const base = Buffer.alloc((1 << 20) + 3, 1);
    const changed = Buffer.from(base);
    changed[base.length - 1] = 2;
    put(join(root, "a"), base);
    put(join(root, "b"), base);
    put(join(root, "c"), changed);
    expect(sameBytes(join(root, "a"), join(root, "b"))).toBe(true);
    expect(sameBytes(join(root, "a"), join(root, "c"))).toBe(false);
  });
});

test("the script drops downloads and links target copies without calling mbx", () => {
  scratch((root) => {
    const home = join(root, "home");
    for (const dir of ["registry", "git", "bin"]) put(join(home, ".cargo", dir, "sentinel"), "x");
    const target = join(home, ".cache/mbx/targets/lib.rlib");
    const blob = join(home, ".cache/mbx/actions/cas/v1/blob");
    put(target, "object");
    put(blob, "object");
    // GC invalidates the pending export group, so any mbx call fails the test.
    put(join(root, "bin/mbx"), "#!/bin/sh\necho mbx-was-called >&2\nexit 1\n", 0o755);
    const path = [join(root, "bin"), dirname(process.execPath), "/usr/bin", "/bin"].join(":");
    const run = headroom({ PATH: path, HOME: home, PHUX_MBX_EXPORT_HEADROOM: "1" });
    expect(run.code, run.stderr).toBe(0);
    expect(run.stderr).not.toContain("mbx-was-called");
    expect(["registry", "git", "bin"].map((dir) => existsSync(join(home, ".cargo", dir)))).toEqual([
      false,
      false,
      true,
    ]);
    expect(statSync(target).ino).toBe(statSync(blob).ino);
  });
});

test.each([
  [{ PHUX_MBX_EXPORT_HEADROOM: "0", HOME: "/nonexistent" }, 0, "skipped outside GitHub Actions"],
  [{ PHUX_MBX_EXPORT_HEADROOM: "1", HOME: "" }, 1, "refusing to clean an empty Cargo home"],
])("the script guards %p", (env, code, message) => {
  const run = headroom({ PATH: "/usr/bin:/bin", ...env });
  expect(run.code).toBe(code);
  expect(run.stdout + run.stderr).toContain(message);
});

test("the script retires pending receipts only when the export cannot fit", () => {
  scratch((root) => {
    const home = join(root, "home");
    const store = join(home, ".cache/mbx/actions");
    const receipt = join(store, "build-receipts/v1/groups/abc/1.json");
    const object = join(store, "cas/v1/blake3/ab/blob");
    put(receipt, "pending");
    put(object, "object");
    put(
      join(root, "bin/df"),
      "#!/bin/sh\necho header\necho '/dev/root 100000000 99999000 1000 100% /'\n",
      0o755,
    );
    put(join(root, "bin/du"), "#!/bin/sh\necho '8000000\t'$2\n", 0o755);
    const env = { HOME: home, PHUX_MBX_EXPORT_HEADROOM: "1" };

    const roomy = headroom({ ...env, PATH: "/usr/bin:/bin" });
    expect(roomy.code, roomy.stderr).toBe(0);
    expect(existsSync(receipt)).toBe(true);

    const full = headroom({ ...env, PATH: `${join(root, "bin")}:/usr/bin:/bin` });
    expect(full.code, full.stderr).toBe(0);
    expect(full.stderr).toContain("not saving this store");
    expect(existsSync(receipt)).toBe(false);
    expect(readFileSync(object, "utf8")).toBe("object");
  });
});
