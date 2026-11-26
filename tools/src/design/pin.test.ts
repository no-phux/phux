// design-pin against local consumer repositories and a fake `gh` on PATH.
import { afterAll, afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Failure, UsageError } from "../lib/command.ts";
import { capture } from "../lib/process.ts";
import { BRANCH, designPin } from "./pin.ts";

const OLD = "1".repeat(40);
const NEW = "2bd".repeat(13) + "e";
const scratch = mkdtempSync(join(tmpdir(), "design-pin-test-"));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

// The consumer's own pin script: writes tokens derived from the revision
// unless TOKENS_FIXED says the tokens did not change.
const PIN_SCRIPT = `import json, os, sys
rev = sys.argv[2]
json.dump({"rev": rev}, open("phux-design.lock.json", "w"))
if not os.environ.get("TOKENS_FIXED"):
    open("tokens.css", "w").write(":root { --rev: " + rev[:4] + "; }\\n")
`;

const FAKE_GH = `#!/bin/sh
printf '%s\\n' "$*" >> "$GH_LOG"
case "$1 $2" in
  "pr list") cat "$GH_OPEN" ;;
  "pr create") echo "https://github.com/no-phux/fixture/pull/1" ;;
  *) exit 9 ;;
esac
`;

const git = (cwd: string, ...args: string[]) => capture(["git", ...args], { cwd }).trim();

let dir = "";
let env: Record<string, string | undefined> = {};

/** A bare consumer remote whose main pins OLD. */
function consumer(name: string): void {
  const work = join(dir, "work", name);
  mkdirSync(join(work, "scripts"), { recursive: true });
  writeFileSync(join(work, "phux-design.lock.json"), JSON.stringify({ rev: OLD }));
  writeFileSync(join(work, "tokens.css"), ":root { --rev: 1111; }\n");
  writeFileSync(join(work, "scripts/phux-design.py"), PIN_SCRIPT);
  git(work, "init", "--quiet", "--initial-branch", "main");
  git(work, "add", "-A");
  git(
    work,
    "-c",
    "user.name=t",
    "-c",
    "user.email=t@example.com",
    "commit",
    "--quiet",
    "-m",
    "init",
  );
  git(dir, "clone", "--quiet", "--bare", work, join(dir, "remotes", `${name}.git`));
}

const remote = (repository: string) => `file://${join(dir, "remotes", `${repository}.git`)}`;

beforeEach(() => {
  dir = mkdtempSync(join(scratch, "case-"));
  mkdirSync(join(dir, "bin"));
  writeFileSync(join(dir, "bin/gh"), FAKE_GH);
  chmodSync(join(dir, "bin/gh"), 0o755);
  writeFileSync(join(dir, "open.json"), "[]");
  writeFileSync(join(dir, "gh.log"), "");
  env = Object.fromEntries(
    ["PATH", "GH_LOG", "GH_OPEN", "GH_TOKEN", "GH_HOST"].map((key) => [key, process.env[key]]),
  );
  process.env["PATH"] = `${join(dir, "bin")}:${process.env["PATH"]}`;
  process.env["GH_LOG"] = join(dir, "gh.log");
  process.env["GH_OPEN"] = join(dir, "open.json");
  // Belt and braces: a real gh, if it were ever reached, cannot authenticate.
  process.env["GH_TOKEN"] = "fixture-invalid";
  process.env["GH_HOST"] = "fixture.invalid";
  delete process.env["TOKENS_FIXED"];
  // Prove the fake answers before any test drives it.
  expect(capture(["sh", "-c", "command -v gh"], { env: process.env }).trim()).toBe(
    join(dir, "bin/gh"),
  );
});

afterEach(() => {
  for (const [key, value] of Object.entries(env)) {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  delete process.env["TOKENS_FIXED"];
});

function pin(repositories: string[]): string[] {
  const lines: string[] = [];
  const log = spyOn(console, "log").mockImplementation((line: string) => void lines.push(line));
  try {
    designPin(NEW, repositories, remote);
  } finally {
    log.mockRestore();
  }
  return lines;
}

const ghCalls = () => readFileSync(join(dir, "gh.log"), "utf8").trim().split("\n").filter(Boolean);
const pushed = (name: string) =>
  git(join(dir, "remotes", `${name}.git`), "branch", "--list", BRANCH);

test("a token change pushes the pin branch and opens a PR", () => {
  consumer("website");
  expect(pin(["website"])).toEqual(["https://github.com/no-phux/fixture/pull/1"]);
  expect(pushed("website")).toContain(BRANCH);
  const message = git(join(dir, "remotes", "website.git"), "log", "-1", "--format=%an|%s", BRANCH);
  expect(message).toBe(
    `PHUX Design Bot|chore(design): pin phux design tokens to ${NEW.slice(0, 12)}`,
  );
  const [list, create] = ghCalls();
  expect(list).toBe(`pr list --repo no-phux/website --head ${BRANCH} --json url`);
  expect(create).toContain(`pr create --repo no-phux/website --base main --head ${BRANCH}`);
  expect(create).toContain(
    `Pin phux design tokens to no-phux/phux@${NEW} (was ${OLD.slice(0, 12)}).`,
  );
});

test("an open pin PR is updated in place, not duplicated", () => {
  consumer("ops");
  writeFileSync(join(dir, "open.json"), '[{"url":"https://example.test/pull/9"}]');
  expect(pin(["ops"])).toEqual([`ops: updated the open pin PR to ${NEW.slice(0, 12)}`]);
  expect(ghCalls()).toHaveLength(1);
});

test("a consumer whose tokens already match is left alone", () => {
  consumer("phux-hud");
  process.env["TOKENS_FIXED"] = "1";
  expect(pin(["phux-hud"])).toEqual([`phux-hud: tokens unchanged since ${OLD.slice(0, 12)}`]);
  expect(pushed("phux-hud")).toBe("");
  expect(ghCalls()).toEqual([]);
});

test("one consumer's failure does not stop the others, and fails the run", () => {
  consumer("website");
  const lines: string[] = [];
  const log = spyOn(console, "log").mockImplementation((line: string) => void lines.push(line));
  try {
    expect(() => designPin(NEW, ["missing", "website"], remote)).toThrow(/^missing: /);
  } finally {
    log.mockRestore();
  }
  expect(lines).toEqual(["https://github.com/no-phux/fixture/pull/1"]);
  expect(() => designPin(NEW, ["missing"], remote)).toThrow(Failure);
});

test("only a full 40-hex revision is accepted", () => {
  for (const revision of [undefined, "", "abc", NEW.toUpperCase(), `${NEW}0`, "../../main"]) {
    expect(() => designPin(revision, [], remote)).toThrow(UsageError);
  }
});
