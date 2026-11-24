// The gate's own contract: log lines, receipts and malformed input. The
// lane-by-lane verdicts run the workflow step in workflows.test.ts,
// ci-gate.test.ts, native-gates.test.ts and web-lane.test.ts.
import { expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { aggregate, parseResults } from "./aggregate.ts";
import { LANES } from "./plan.ts";

const outputs = Object.fromEntries(
  [...LANES, "rune_qualify", "release_qualify"].map((key) => [key, "false"]),
);
const passing = {
  plan: { result: "success", outputs },
  lint: { result: "skipped" },
  "queue-apple": { result: "skipped" },
};
const all = ["guards", "lint", "test-build", "tests", "e2e", "ffi", "integrations"];
const complete = {
  ...Object.fromEntries(
    [...all, "web-vt", "web-client", "native", "icount", "rune", "queue-apple"].map((id) => [
      id,
      { result: "skipped" },
    ]),
  ),
  plan: { result: "success", outputs },
};

function cli(env: Record<string, string>) {
  const dir = mkdtempSync(join(tmpdir(), "ci-aggregate-"));
  try {
    const proc = Bun.spawnSync(["bun", "tools/main.ts", "ci-aggregate"], {
      cwd: repoPath("."),
      env: { ...process.env, RUNNER_TEMP: dir, ...env },
      stdout: "pipe",
      stderr: "pipe",
    });
    const receipt = join(dir, "receipt/receipt.json");
    return {
      code: proc.exitCode,
      stdout: proc.stdout.toString(),
      receipt: existsSync(receipt) ? readFileSync(receipt, "utf8") : undefined,
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("every job's result is logged before the verdict", () => {
  const verdict = aggregate({ results: complete, event: "pull_request", draft: false });
  expect(verdict.ok).toBe(true);
  expect(verdict.lines).toContain("success\tplan");
  expect(verdict.lines).toContain("skipped\tlint");
  const missing = aggregate({ results: passing, event: "pull_request", draft: false });
  expect(missing.ok).toBe(false);
  expect(missing.lines).toContain("::error::guards is expected and was missing");
});

test("a pass writes the receipt; a failure or a draft never does", () => {
  const pass = cli({ RESULTS: JSON.stringify(complete), EVENT_NAME: "pull_request", RECEIPT: "{}" });
  expect([pass.code, pass.receipt]).toEqual([0, "{}\n"]);
  const failed = { ...complete, lint: { result: "failure" } };
  const fail = cli({ RESULTS: JSON.stringify(failed), EVENT_NAME: "pull_request", RECEIPT: "{}" });
  expect([fail.code, fail.receipt]).toEqual([1, undefined]);
  expect(fail.stdout).toContain("::error::lint neither passed nor skipped");
  const idle = { plan: { result: "skipped" } };
  const draft = cli({ RESULTS: JSON.stringify(idle), DRAFT: "true", RECEIPT: "{}" });
  expect([draft.code, draft.receipt]).toEqual([0, undefined]);
  expect(draft.stdout).toContain("::notice::draft PR: lanes skipped");
});

test("absent or malformed RESULTS fails the gate", () => {
  expect(cli({ RESULTS: "" }).code).toBe(1);
  expect(cli({ RESULTS: "[1]" }).code).toBe(1);
  expect(cli({ RESULTS: "not json" }).code).toBe(1);
  expect(parseResults("[]")).toBeUndefined();
  expect(parseResults('{"plan":{}}')).toEqual({ plan: {} });
});
