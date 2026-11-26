// main-red against a fake `gh` on PATH that logs each call and answers from
// fixture files.
import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";

const scratch = mkdtempSync(join(tmpdir(), "phux-main-red-"));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

const bin = join(scratch, "bin");
const state = join(scratch, "state");
mkdirSync(bin);
writeFileSync(
  join(bin, "gh"),
  `#!/bin/sh
printf '%s\\n' "$*" >>"$STATE/calls"
case "$1 $2" in
  "issue list") cat "$STATE/open" 2>/dev/null || true ;;
  "api repos/o/r/actions/runs?branch=main&event=push&status=success&per_page=50") cat "$STATE/green" ;;
  "api repos/o/r/actions/runs?branch=main&event=push&status=completed&per_page=50") cat "$STATE/latest" ;;
  "api repos/o/r/compare/"*) cat "$STATE/compare" ;;
  "api repos/o/r/commits/"*) cat "$STATE/commit" ;;
esac
`,
  { mode: 0o755 },
);

const commit = (sha: string, message: string) => ({ sha, commit: { message } });
const runs = (...entries: Record<string, string>[]) => JSON.stringify({ workflow_runs: entries });

/** Fresh fixture state, then one main-red run for `conclusion`; returns the gh calls. */
function run(conclusion: string, files: Record<string, string>): string {
  rmSync(state, { recursive: true, force: true });
  mkdirSync(state);
  writeFileSync(join(state, "calls"), "");
  writeFileSync(join(state, "green"), runs());
  writeFileSync(join(state, "latest"), runs());
  const head = commit("cccccccccccccccc", "fix: only commit (#3)\n\nbody");
  writeFileSync(join(state, "commit"), JSON.stringify(head));
  for (const [name, text] of Object.entries(files)) writeFileSync(join(state, name), text);
  const result = Bun.spawnSync([process.execPath, repoPath("tools/main.ts"), "main-red"], {
    env: {
      ...process.env,
      PATH: `${bin}:${process.env["PATH"] ?? ""}`,
      STATE: state,
      GH_REPO: "o/r",
      WORKFLOW_NAME: "ci",
      RUN_URL: "https://example.test/run",
      RUN_CREATED_AT: "2026-11-14T12:00:00Z",
      HEAD_SHA: "cccccccccccccccc",
      CONCLUSION: conclusion,
      TRACKED_WORKFLOWS: "",
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  expect(result.exitCode, result.stderr.toString()).toBe(0);
  return readFileSync(join(state, "calls"), "utf8");
}

const green = runs(
  { name: "ci", created_at: "2026-11-14T13:00:00Z", head_sha: "later000000" },
  { name: "rune-macos", created_at: "2026-11-14T11:30:00Z", head_sha: "other000000" },
  { name: "ci", created_at: "2026-11-14T11:00:00Z", head_sha: "aaaaaaaaaaaa" },
);
const compare = JSON.stringify({
  commits: [
    commit("aaaaaaaaaaaa1", "chore: older (#1)"),
    commit("bbbbbbbbbbbb2", "feat: suspect (#2)\n\nbody"),
  ],
});

test("a red run with no open issue opens one naming the suspects since green", () => {
  const calls = run("failure", { green, compare });
  expect(calls).toMatch(
    /^issue create --label ci\/main-red --title main is red: ci at cccccccccc --body/m,
  );
  expect(calls).toContain(
    "- bbbbbbbbbb feat: suspect (#2)\n- aaaaaaaaaa chore: older (#1)\n\nLast green `ci` push: `aaaaaaaaaa`.",
  );
  expect(calls).toContain("api repos/o/r/compare/aaaaaaaaaaaa...cccccccccccccccc");
});

test("a red run with no earlier green run names the head commit alone", () => {
  expect(run("timed_out", {})).toContain("- cccccccccc fix: only commit (#3)\n");
});

test("a red run with an open issue comments instead of opening another", () => {
  const calls = run("failure", { open: "7\n", green, compare });
  expect(calls).toMatch(/^issue comment 7 /m);
  expect(calls).not.toMatch(/^issue create/m);
});

test("green closes the issue only when every tracked workflow is green", () => {
  const latest = (conclusion: string) =>
    runs(
      { name: "ci", conclusion: "cancelled" },
      { name: "ci", conclusion },
      { name: "rune-macos", conclusion: "success" },
    );
  expect(run("success", { open: "7\n", latest: latest("success") })).toMatch(
    /^issue close 7 --comment Green again at `cccccccccc`: every tracked workflow \(ci rune-macos\)/m,
  );
  expect(run("success", { open: "7\n", latest: latest("failure") })).not.toMatch(/^issue close/m);
});

test("green with no open issue, and cancelled runs, write nothing", () => {
  for (const conclusion of ["success", "cancelled"]) {
    expect(run(conclusion, {})).not.toMatch(/^issue (create|comment|close)/m);
  }
});
