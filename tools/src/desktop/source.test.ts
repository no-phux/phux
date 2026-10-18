import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import {
  bootstrap,
  exportSeries,
  fetchCrate,
  layout,
  parsePin,
  pinnedPatches,
  readPin,
  sameTree,
  sha256,
  SourceError,
  type Layout,
} from "./source.ts";

const PATCH = `diff --git a/demo/src/lib.rs b/demo/src/lib.rs
--- a/demo/src/lib.rs
+++ b/demo/src/lib.rs
@@ -1 +1 @@
-pub fn answer() -> u32 { 41 }
+pub fn answer() -> u32 { 42 }
`;

async function crateArchive(): Promise<Uint8Array> {
  const archive = new Bun.Archive(
    {
      "demo-0.1.0/Cargo.toml": '[package]\nname = "demo"\nversion = "0.1.0"\n',
      "demo-0.1.0/src/lib.rs": "pub fn answer() -> u32 { 41 }\n",
    },
    { compress: "gzip" },
  );
  return new Uint8Array(await (await archive.blob()).arrayBuffer());
}

/** The message a promise rejects with; resolving is a test failure. */
async function failure(promise: Promise<unknown>): Promise<string> {
  try {
    await promise;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  throw new Error("expected a rejection");
}

let scratch: string;
let paths: Layout;
let archive: Uint8Array;
let requests: number;
let server: ReturnType<typeof Bun.serve>;

function writePin(overrides: { crateSha?: string; patchSha?: string } = {}): void {
  const pin = {
    registry: `http://127.0.0.1:${server.port}/crates`,
    crates: { demo: { version: "0.1.0", sha256: overrides.crateSha ?? sha256(archive) } },
    patches: { "patches/0001-answer.patch": overrides.patchSha ?? sha256(PATCH) },
  };
  writeFileSync(join(paths.toolchain, "source.json"), `${JSON.stringify(pin, null, 2)}\n`);
}

beforeEach(async () => {
  scratch = mkdtempSync(join(tmpdir(), "phux-desktop-source-test-"));
  paths = layout(join(scratch, "desktop/toolchain"));
  mkdirSync(join(paths.toolchain, "patches"), { recursive: true });
  writeFileSync(join(paths.toolchain, "patches/0001-answer.patch"), PATCH);
  archive = await crateArchive();
  requests = 0;
  server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      requests += 1;
      const ok = new URL(request.url).pathname === "/crates/demo/demo-0.1.0.crate";
      return ok ? new Response(archive) : new Response("missing", { status: 404 });
    },
  });
  writePin();
});

afterEach(async () => {
  await server.stop(true);
  rmSync(scratch, { recursive: true, force: true });
});

describe("pins", () => {
  test("a malformed source.json is named, not trusted", () => {
    expect(() => parsePin({ registry: 1, crates: {}, patches: {} })).toThrow(SourceError);
    expect(() => parsePin({ registry: "r", crates: { x: { version: "1" } }, patches: {} })).toThrow(
      "x.sha256",
    );
  });

  test("a patch that does not match its pin fails", () => {
    writePin({ patchSha: "0".repeat(64) });
    expect(() => pinnedPatches(readPin(paths), paths.toolchain)).toThrow(
      "patch checksum mismatch: patches/0001-answer.patch",
    );
  });
});

describe("fetchCrate", () => {
  test("a download that does not match its pin fails and is not cached", async () => {
    const pin = readPin(paths);
    const wrong = { version: "0.1.0", sha256: "0".repeat(64) };
    expect(await failure(fetchCrate(pin.registry, "demo", wrong, paths.cache))).toContain(
      "does not match its pinned sha256",
    );
    expect(existsSync(join(paths.cache, "demo-0.1.0.crate"))).toBe(false);
  });

  test("a verified download is cached and reused without the network", async () => {
    const pin = readPin(paths);
    const crate = pin.crates["demo"];
    if (crate === undefined) throw new Error("fixture pin lost its crate");
    expect(await fetchCrate(pin.registry, "demo", crate, paths.cache)).toEqual(archive);
    expect(await fetchCrate(pin.registry, "demo", crate, paths.cache)).toEqual(archive);
    expect(requests).toBe(1);
  });

  test("a cached archive that does not match its pin is preserved and fails", async () => {
    mkdirSync(paths.cache, { recursive: true });
    const cached = join(paths.cache, "demo-0.1.0.crate");
    writeFileSync(cached, "tampered");
    const pin = readPin(paths);
    const crate = { version: "0.1.0", sha256: sha256(archive) };
    expect(await failure(fetchCrate(pin.registry, "demo", crate, paths.cache))).toContain(
      "does not match its pin; preserve it",
    );
    expect(readFileSync(cached, "utf8")).toBe("tampered");
    expect(requests).toBe(0);
  });
});

describe("bootstrap", () => {
  test("materializes the patched crates, then verifies them", async () => {
    expect(await bootstrap("materialize", paths)).toContain("materialized demo 0.1.0 with 1 patch");
    expect(readFileSync(join(paths.output, "demo/src/lib.rs"), "utf8")).toContain("{ 42 }");
    expect(await bootstrap("check", paths)).toContain("verified pinned GPUI source");
  });

  test("--check fails when the tree is missing and writes nothing", async () => {
    expect(await failure(bootstrap("check", paths))).toContain(
      "is missing; run just desktop-source",
    );
    expect(existsSync(paths.output)).toBe(false);
  });

  test("an edited tree is never overwritten", async () => {
    await bootstrap("materialize", paths);
    const source = join(paths.output, "demo/src/lib.rs");
    writeFileSync(source, "local edit\n");
    expect(await failure(bootstrap("materialize", paths))).toContain("differs from the pinned");
    expect(readFileSync(source, "utf8")).toBe("local edit\n");
    expect(await failure(bootstrap("edit", paths))).toContain("has unrecorded edits");
  });

  test("--edit then --export round-trips the series byte for byte", async () => {
    await bootstrap("materialize", paths);
    await bootstrap("edit", paths);
    expect(existsSync(join(paths.output, ".git"))).toBe(true);
    // The first export writes git's own diff form (index lines included).
    expect(exportSeries(readPin(paths), paths)).toBe(1);
    const patch = join(paths.toolchain, "patches/0001-answer.patch");
    const exported = readFileSync(patch, "utf8");
    expect(exported).toContain("+pub fn answer() -> u32 { 42 }");
    const pin = readFileSync(join(paths.toolchain, "source.json"), "utf8");
    rmSync(paths.output, { recursive: true });
    await bootstrap("edit", paths);
    exportSeries(readPin(paths), paths);
    expect(readFileSync(patch, "utf8")).toBe(exported);
    expect(readFileSync(join(paths.toolchain, "source.json"), "utf8")).toBe(pin);
  });

  test("--export writes new commits as patches and drops stale ones", async () => {
    await bootstrap("edit", paths);
    writeFileSync(join(paths.toolchain, "patches/0009-stale.patch"), "stale");
    writeFileSync(join(paths.output, "demo/NEW"), "added\n");
    git(["add", "-A"], paths.output);
    git(["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "0002-new"], paths.output);
    expect(exportSeries(readPin(paths), paths)).toBe(2);
    const pin = readPin(paths);
    expect(Object.keys(pin.patches)).toEqual([
      "patches/0001-answer.patch",
      "patches/0002-new.patch",
    ]);
    expect(existsSync(join(paths.toolchain, "patches/0009-stale.patch"))).toBe(false);
    expect(pinnedPatches(pin, paths.toolchain)).toHaveLength(2);
  });

  test("--export refuses a dirty or non-edit checkout", async () => {
    await bootstrap("materialize", paths);
    expect(() => exportSeries(readPin(paths), paths)).toThrow("is not an --edit checkout");
    rmSync(paths.output, { recursive: true });
    await bootstrap("edit", paths);
    writeFileSync(join(paths.output, "demo/dirty"), "x");
    expect(() => exportSeries(readPin(paths), paths)).toThrow("commit or discard the changes");
  });
});

describe("sameTree", () => {
  function tree(name: string, files: Record<string, string>): string {
    const root = join(scratch, name);
    for (const [path, text] of Object.entries(files)) {
      mkdirSync(join(root, path, ".."), { recursive: true });
      writeFileSync(join(root, path), text);
    }
    mkdirSync(root, { recursive: true });
    return root;
  }

  test("ignores .git and compares content at every level", () => {
    const left = tree("left", { "a/b": "1", c: "2", ".git/HEAD": "x" });
    expect(sameTree(left, tree("right", { "a/b": "1", c: "2" }))).toBe(true);
    expect(sameTree(left, tree("content", { "a/b": "3", c: "2" }))).toBe(false);
    expect(sameTree(left, tree("extra", { "a/b": "1", c: "2", d: "" }))).toBe(false);
    expect(sameTree(left, tree("missing", { "a/b": "1" }))).toBe(false);
    expect(sameTree(left, tree("kind", { a: "1", c: "2" }))).toBe(false);
  });

  test("a dangling symlink is a difference", () => {
    const left = tree("l", { a: "1" });
    const right = tree("r", {});
    symlinkSync(join(scratch, "nowhere"), join(right, "a"));
    expect(sameTree(left, right)).toBe(false);
  });
});
