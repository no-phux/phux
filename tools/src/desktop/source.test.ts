import { afterAll, afterEach, beforeEach, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  symlinkSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { git } from "../lib/git.ts";
import {
  bootstrap,
  exportSeries,
  fetchCrate,
  layout,
  pinnedPatches,
  readPin,
  sameTree,
  sha256,
  type Layout,
} from "./source.ts";

const PATCH = `diff --git a/demo/src/lib.rs b/demo/src/lib.rs
--- a/demo/src/lib.rs
+++ b/demo/src/lib.rs
@@ -1 +1 @@
-pub fn answer() -> u32 { 41 }
+pub fn answer() -> u32 { 42 }
`;
const ARCHIVE = await new Bun.Archive(
  {
    "demo-0.1.0/Cargo.toml": '[package]\nname = "demo"\nversion = "0.1.0"\n',
    "demo-0.1.0/src/lib.rs": "pub fn answer() -> u32 { 41 }\n",
  },
  { compress: "gzip" },
).bytes();

let requests = 0;
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request) {
    requests += 1;
    const ok = new URL(request.url).pathname === "/crates/demo/demo-0.1.0.crate";
    return ok ? new Response(ARCHIVE) : new Response("missing", { status: 404 });
  },
});
afterAll(() => server.stop(true));

let scratch: string;
let paths: Layout;

function write(path: string, text: string): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}
const read = (path: string) => readFileSync(join(paths.toolchain, path), "utf8");

function writePin(patchSha = sha256(PATCH)): void {
  const pin = {
    registry: `http://127.0.0.1:${server.port}/crates`,
    crates: { demo: { version: "0.1.0", sha256: sha256(ARCHIVE) } },
    patches: { "patches/0001-answer.patch": patchSha },
  };
  writeFileSync(join(paths.toolchain, "source.json"), `${JSON.stringify(pin, null, 2)}\n`);
}

beforeEach(() => {
  scratch = mkdtempSync(join(tmpdir(), "phux-desktop-source-test-"));
  // Inside a repository, as the real toolchain is: `git apply` must still
  // resolve patch paths against the scratch tree, not the enclosing checkout.
  git(["init", "-q"], scratch);
  paths = layout(join(scratch, "desktop/toolchain"));
  write(join(paths.toolchain, "patches/0001-answer.patch"), PATCH);
  requests = 0;
  writePin();
});
afterEach(() => rmSync(scratch, { recursive: true, force: true }));

/** What a promise rejects with; resolving is a test failure. */
const rejection = (promise: Promise<unknown>) =>
  promise.then(
    () => "resolved",
    (error: unknown) => String(error),
  );

test("a patch that does not match its pin fails", () => {
  writePin("0".repeat(64));
  expect(() => pinnedPatches(readPin(paths), paths.toolchain)).toThrow(
    "patch checksum mismatch: patches/0001-answer.patch",
  );
});

test("downloads are verified before caching; a tampered cache is preserved", async () => {
  const cached = join(paths.cache, "crates/demo-0.1.0.crate");
  const demo = (sha = sha256(ARCHIVE)) =>
    fetchCrate(readPin(paths).registry, "demo", { version: "0.1.0", sha256: sha }, paths.cache);
  expect(await rejection(demo("0".repeat(64)))).toContain("does not match its pinned sha256");
  expect(existsSync(cached)).toBe(false);
  expect(await demo()).toEqual(ARCHIVE);
  expect(await demo()).toEqual(ARCHIVE);
  expect(requests).toBe(2);
  writeFileSync(cached, "tampered");
  expect(await rejection(demo())).toContain("does not match its pin; preserve it");
  expect(readFileSync(cached, "utf8")).toBe("tampered");
  expect(requests).toBe(2);
});

test("--check needs a tree; materialize writes one that verifies and --edit adopts", async () => {
  expect(await rejection(bootstrap("check", paths))).toContain(
    "is missing; run just desktop-source",
  );
  expect(existsSync(paths.output)).toBe(false);
  expect(await bootstrap("materialize", paths)).toContain("materialized demo 0.1.0 with 1 patch");
  expect(readFileSync(join(paths.output, "demo/src/lib.rs"), "utf8")).toContain("{ 42 }");
  expect(existsSync(join(paths.output, ".git"))).toBe(false);
  expect(await bootstrap("check", paths)).toContain("verified pinned GPUI source");
  expect(() => exportSeries(readPin(paths), paths)).toThrow("is not an --edit checkout");
  await bootstrap("edit", paths);
  expect(existsSync(join(paths.output, ".git"))).toBe(true);
});

test("an edited tree is never overwritten", async () => {
  await bootstrap("materialize", paths);
  const source = join(paths.output, "demo/src/lib.rs");
  writeFileSync(source, "local edit\n");
  expect(await rejection(bootstrap("materialize", paths))).toContain("differs from the pinned");
  expect(await rejection(bootstrap("edit", paths))).toContain("has unrecorded edits");
  expect(readFileSync(source, "utf8")).toBe("local edit\n");
});

test("--export round-trips the series, adds commits and drops stale patches", async () => {
  const exportPin = () => exportSeries(readPin(paths), paths);
  await bootstrap("edit", paths);
  // The first export writes git's own diff form (index lines included).
  expect(exportPin()).toBe(1);
  const [patch, pin] = [read("patches/0001-answer.patch"), read("source.json")];
  expect(patch).toContain("+pub fn answer() -> u32 { 42 }");
  rmSync(paths.output, { recursive: true });
  await bootstrap("edit", paths);
  exportPin();
  expect([read("patches/0001-answer.patch"), read("source.json")]).toEqual([patch, pin]);

  writeFileSync(join(paths.toolchain, "patches/0009-stale.patch"), "stale");
  writeFileSync(join(paths.output, "demo/NEW"), "added\n");
  git(["add", "-A"], paths.output);
  git(["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "0002-new"], paths.output);
  expect(exportPin()).toBe(2);
  const patches = Object.keys(readPin(paths).patches);
  expect(patches).toEqual(["patches/0001-answer.patch", "patches/0002-new.patch"]);
  expect(existsSync(join(paths.toolchain, "patches/0009-stale.patch"))).toBe(false);
  expect(pinnedPatches(readPin(paths), paths.toolchain)).toHaveLength(2);
  writeFileSync(join(paths.output, "demo/dirty"), "x");
  expect(exportPin).toThrow("commit or discard the changes");
});

test("sameTree ignores .git and compares names, kinds and bytes at every level", () => {
  const tree = (name: string, files: Record<string, string>) => {
    mkdirSync(join(scratch, name));
    for (const [path, text] of Object.entries(files)) write(join(scratch, name, path), text);
    return join(scratch, name);
  };
  const left = tree("left", { "a/b": "1", c: "2", ".git/HEAD": "x" });
  expect(sameTree(left, tree("right", { "a/b": "1", c: "2" }))).toBe(true);
  for (const [name, files] of Object.entries({
    content: { "a/b": "3", c: "2" },
    extra: { "a/b": "1", c: "2", d: "" },
    missing: { "a/b": "1" },
    kind: { a: "1", c: "2" },
  })) {
    expect([name, sameTree(left, tree(name, files))]).toEqual([name, false]);
  }
  const dangling = tree("dangling", { c: "2" });
  mkdirSync(join(dangling, "a"));
  symlinkSync(join(scratch, "nowhere"), join(dangling, "a/b"));
  expect(sameTree(left, dangling)).toBe(false);
});
