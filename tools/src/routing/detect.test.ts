import { afterAll, afterEach, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { WORKSPACE_LOCKS } from "../cargo/release-orchestration.ts";
import { git } from "../lib/git.ts";
import { detect, docsCatalogChanged, type Detection } from "./detect.ts";

const template = mkdtempSync(join(tmpdir(), "phux-routing-"));
// The payload lives outside the fixture tree, exactly like Actions.
const payloads = mkdtempSync(join(tmpdir(), "phux-events-"));
let repo = template;
git(["init", "--quiet"], template);
const settings = [
  "-c",
  "user.email=routing@example.invalid",
  "-c",
  "user.name=Routing Test",
  "-c",
  "core.hooksPath=/dev/null",
  "-c",
  "commit.gpgsign=false",
];
const base = commitFiles({}, "HEAD");

beforeEach(() => {
  repo = mkdtempSync(join(tmpdir(), "phux-routing-case-"));
  // A failed command must not leave staged or untracked paths for another test.
  git(["clone", "--quiet", "--local", ...settings, template, repo], template);
});

afterEach(() => {
  if (repo !== template) rmSync(repo, { recursive: true, force: true });
  repo = template;
});

afterAll(() => {
  rmSync(template, { recursive: true, force: true });
  rmSync(payloads, { recursive: true, force: true });
});

function commitFiles(files: Record<string, string>, from = base): string {
  if (from !== "HEAD") git(["checkout", "--quiet", "--detach", from], repo);
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(repo, path)), { recursive: true });
    writeFileSync(join(repo, path), text);
  }
  git(["add", "--all"], repo);
  git([...settings, "commit", "--quiet", "--allow-empty", "-m", "fixture"], repo);
  return git(["rev-parse", "HEAD"], repo).trim();
}

function run(name: string, event: unknown): Detection {
  const path = join(payloads, `${name}-${Bun.randomUUIDv7()}.json`);
  writeFileSync(path, JSON.stringify(event));
  return detect(name, path, repo);
}

const push = (before: string, after: string) => run("push", { before, after });

test("push and pull_request resolve to the same paths", () => {
  const paths = ["crates/phux-server/src/lib.rs", "integrations/pi/src/index.ts"];
  const head = commitFiles(Object.fromEntries(paths.map((path) => [path, "fixture\n"])));
  const events = {
    push: { before: base, after: head },
    pull_request: { pull_request: { base: { sha: base }, head: { sha: head } } },
  };
  for (const [name, event] of Object.entries(events)) {
    expect(run(name, event)).toEqual({ kind: "paths", paths, docsCatalog: false });
  }
});

test("a merge group diffs its queue commit against the group's base", () => {
  const earlier = commitFiles({ "docs/development.md": "fixture\n" });
  const queued = commitFiles({ "clients/rune/src/main.rs": "fixture\n" }, "HEAD");
  const event = { merge_group: { base_sha: base, head_sha: queued } };
  expect(run("merge_group", event)).toEqual({
    kind: "paths",
    paths: ["clients/rune/src/main.rs", "docs/development.md"],
    docsCatalog: true,
  });
  expect(run("merge_group", { merge_group: { base_sha: earlier } }).kind).toBe("full");
});

test("a pull request diffs from the merge base, not the advanced base branch", () => {
  const head = commitFiles({ "docs/development.md": "fixture\n" });
  const advanced = commitFiles({ "new-product/unrelated.xyz": "fixture\n" });
  const event = { pull_request: { base: { sha: advanced }, head: { sha: head } } };
  expect(run("pull_request", event)).toEqual({
    kind: "paths",
    paths: ["docs/development.md"],
    docsCatalog: true,
  });
});

test("unknown history, unrepresentable paths and explicit events request full validation", () => {
  const cases: [string, unknown][] = [
    ["workflow_dispatch", {}],
    ["schedule", {}],
    ["new_event", {}],
    ["push", { before: "0".repeat(40), after: base }],
    ["push", { before: "f".repeat(40), after: base }],
    ["push", { before: "--help", after: base }],
    ["push", { before: base, after: commitFiles({ "docs/line\nbreak.md": "fixture\n" }) }],
    ["pull_request", {}],
    ["merge_group", {}],
    ["push", []],
  ];
  for (const [name, event] of cases) expect(run(name, event).kind).toBe("full");
  expect(detect("push", undefined, repo).kind).toBe("full");
});

test("a rename reports both sides", () => {
  const before = commitFiles({ "integrations/pi/fixture.txt": "fixture\n" });
  mkdirSync(join(repo, "clients/phux-web"), { recursive: true });
  git(["mv", "integrations/pi/fixture.txt", "clients/phux-web/fixture.txt"], repo);
  expect(push(before, commitFiles({}, "HEAD"))).toEqual({
    kind: "paths",
    paths: ["clients/phux-web/fixture.txt", "integrations/pi/fixture.txt"],
    docsCatalog: false,
  });
});

test("flat docs adds, renames, and deletes select the catalog check", () => {
  const added = commitFiles({ "docs/zz-uncatalogued-probe.md": "# probe\n" }, base);
  expect(push(base, added)).toEqual({
    kind: "paths",
    paths: ["docs/zz-uncatalogued-probe.md"],
    docsCatalog: true,
  });
  const edited = commitFiles({ "docs/zz-uncatalogued-probe.md": "# probe\n\nedited\n" }, added);
  expect(push(added, edited)).toMatchObject({ kind: "paths", docsCatalog: false });
  expect(docsCatalogChanged(added, edited, repo)).toBe(false);

  const nested = commitFiles({ "docs/reference/zz-probe.md": "# nested\n" }, base);
  expect(push(base, nested)).toMatchObject({ kind: "paths", docsCatalog: false });

  const named = commitFiles({ "docs/old-page.md": "# page\n" }, base);
  mkdirSync(join(repo, "docs/reference"), { recursive: true });
  git(["mv", "docs/old-page.md", "docs/reference/old-page.md"], repo);
  const moved = commitFiles({}, "HEAD");
  expect(docsCatalogChanged(named, moved, repo)).toBe(true);

  const flat = commitFiles({ "docs/renamed-page.md": "# page\n" }, base);
  git(["mv", "docs/renamed-page.md", "docs/new-page.md"], repo);
  const renamed = commitFiles({}, "HEAD");
  expect(docsCatalogChanged(flat, renamed, repo)).toBe(true);
  git(["rm", "--quiet", "docs/new-page.md"], repo);
  const removed = commitFiles({}, "HEAD");
  expect(docsCatalogChanged(renamed, removed, repo)).toBe(true);
});

// Exercises every standalone lock and several real Git checkouts; keep a
// bounded integration-test deadline rather than the five-second unit default.
test("PR2406's root and OpenCode release metadata requests no compile lane", () => {
  const lock = 'version = 3\n\n[[package]]\nname = "phux"\nversion = "0.145.0"\n';
  const skill = '  version: "0.145.0" # x-release-please-version\n';
  const pkg = JSON.stringify({
    name: "@phux/opencode",
    version: "0.9.1",
    scripts: { compile: "bun scripts/build.mjs" },
    dependencies: { "@opencode/plugin": "2.2.6" },
    exports: { ".": { import: "./index.js" } },
  });
  const files: Record<string, string> = {
    "CHANGELOG.md": "# changelog\n",
    "Cargo.toml": '[workspace.package]\nversion = "0.145.0"\n',
    ...Object.fromEntries(WORKSPACE_LOCKS.map((path) => [path, lock])),
    ".release-please-manifest.json": `${JSON.stringify({
      ".": "0.145.0",
      "integrations/opencode-v2": "0.9.1",
      "integrations/pi": "0.9.2",
    })}\n`,
    ".agents/skills/using-phux/SKILL.md": skill,
    ".agents/skills/using-phux-mcp/SKILL.md": skill,
    "integrations/opencode-v2/package.json": pkg,
    "integrations/opencode-v2/CHANGELOG.md": "# changelog\n",
  };
  const before = commitFiles(files);
  const bumped = Object.fromEntries(
    Object.entries(files).map(([path, text]) => [
      path,
      text.replaceAll("0.145.0", "0.146.0").replaceAll("0.9.1", "0.9.2"),
    ]),
  );
  bumped["CHANGELOG.md"] = "# changelog\n\n## 0.146.0\n";
  bumped["integrations/opencode-v2/CHANGELOG.md"] = "# changelog\n\n## 0.9.2\n";
  const head = commitFiles(bumped, before);
  expect(push(before, head)).toEqual({ kind: "release-metadata" });
  expect(
    run("pull_request", { pull_request: { base: { sha: before }, head: { sha: head } } }),
  ).toEqual({ kind: "release-metadata" });
  expect(run("merge_group", { merge_group: { base_sha: before, head_sha: head } })).toEqual({
    kind: "release-metadata",
  });
  for (const path of [
    "crates/phux/src/main.rs",
    "crates/phux-protocol/src/wire.rs",
    "build.rs",
    "integrations/opencode-v2/index.js",
    "release-please-config.json",
    ".github/workflows/ci.yml",
  ]) {
    const touched = commitFiles({ [path]: "runtime or policy change\n" }, head);
    expect(push(before, touched).kind).toBe("paths");
  }
  // A bump that leaves a lock behind fails --locked builds: run them.
  const stale = commitFiles({ "clients/rune/Cargo.lock": lock }, head);
  expect(push(before, stale).kind).toBe("paths");
}, 15_000);
