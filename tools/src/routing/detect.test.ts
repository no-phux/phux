import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { git } from "../lib/git.ts";
import { detect, type Detection } from "./detect.ts";

let repo = "";
let base = "";
const payloads = mkdtempSync(join(tmpdir(), "phux-events-"));

function commitFiles(files: Record<string, string>, from = base, message = "fixture"): string {
  git(["checkout", "--quiet", "--detach", from], repo);
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(repo, path)), { recursive: true });
    writeFileSync(join(repo, path), text);
  }
  git(["add", "--all"], repo);
  git(["commit", "--quiet", "--allow-empty", "-m", message], repo);
  return git(["rev-parse", "HEAD"], repo).trim();
}

function run(name: string, event: unknown): Detection {
  // The payload lives outside the fixture tree, exactly like Actions.
  const path = join(payloads, `${name}-${Bun.randomUUIDv7()}.json`);
  writeFileSync(path, JSON.stringify(event));
  return detect(name, path, repo);
}

beforeAll(() => {
  repo = mkdtempSync(join(tmpdir(), "phux-routing-"));
  git(["init", "--quiet"], repo);
  git(["config", "user.email", "routing@example.invalid"], repo);
  git(["config", "user.name", "Routing Test"], repo);
  git(["config", "core.hooksPath", "/dev/null"], repo);
  git(["commit", "--quiet", "--allow-empty", "-m", "base"], repo);
  base = git(["rev-parse", "HEAD"], repo).trim();
});

afterAll(() => {
  rmSync(repo, { recursive: true, force: true });
  rmSync(payloads, { recursive: true, force: true });
});

test("push, pull_request and merge_group resolve to the same paths", () => {
  for (const paths of [
    ["docs/SETUP.md"],
    ["crates/phux-server/src/lib.rs", "integrations/pi/src/index.ts"],
  ]) {
    const head = commitFiles(Object.fromEntries(paths.map((path) => [path, "fixture\n"])));
    const events = {
      push: { before: base, after: head },
      pull_request: { pull_request: { base: { sha: base }, head: { sha: head } } },
      merge_group: { merge_group: { base_sha: base, head_sha: head } },
    };
    for (const [name, event] of Object.entries(events)) {
      expect(run(name, event)).toEqual({ kind: "paths", paths: paths.toSorted() });
    }
  }
});

test("a pull request diffs from the merge base, not the advanced base branch", () => {
  const head = commitFiles({ "docs/SETUP.md": "fixture\n" });
  const advanced = commitFiles({ "new-product/unrelated.xyz": "fixture\n" });
  const event = { pull_request: { base: { sha: advanced }, head: { sha: head } } };
  expect(run("pull_request", event)).toEqual({ kind: "paths", paths: ["docs/SETUP.md"] });
});

test("unknown history and explicit events request full validation", () => {
  const cases: [string, unknown][] = [
    ["workflow_dispatch", {}],
    ["schedule", {}],
    ["new_event", {}],
    ["push", { before: "0".repeat(40), after: base }],
    ["push", { before: "f".repeat(40), after: base }],
    ["push", { before: "--help", after: base }],
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
  git(["commit", "--quiet", "-m", "move"], repo);
  const after = git(["rev-parse", "HEAD"], repo).trim();
  expect(run("push", { before, after })).toEqual({
    kind: "paths",
    paths: ["clients/phux-web/fixture.txt", "integrations/pi/fixture.txt"],
  });
});

test("a filename an output line cannot carry requests full validation", () => {
  const head = commitFiles({ "docs/line\nbreak.md": "fixture\n" });
  expect(run("push", { before: base, after: head }).kind).toBe("full");
});

test("a release-please version bump requests no compile lane", () => {
  const files: Record<string, string> = {
    "CHANGELOG.md": "# changelog\n",
    "Cargo.toml": '[workspace.package]\nversion = "0.1.0"\n',
    "Cargo.lock": 'version = 3\n\n[[package]]\nname = "phux"\nversion = "0.1.0"\n',
    ".release-please-manifest.json": `${JSON.stringify({ ".": "0.1.0" })}\n`,
    ".agents/skills/using-phux/SKILL.md": '  version: "0.1.0" # x-release-please-version\n',
  };
  const before = commitFiles(files);
  const bumped = Object.fromEntries(
    Object.entries(files).map(([path, text]) => [path, text.replaceAll("0.1.0", "0.2.0")]),
  );
  const after = commitFiles(bumped, before, "chore: release main");
  expect(run("push", { before, after })).toEqual({ kind: "release-metadata" });
  const touched = commitFiles({ ...bumped, "crates/phux/src/main.rs": "fn main() {}\n" }, before);
  expect(run("push", { before, after: touched }).kind).toBe("paths");
});
