// The pull-request exception for native setup and rust icount, and the
// scripts that job actually runs. workflows.test.ts keeps the shared
// success-or-skip cases; this file is the event split.
import { expect, test } from "bun:test";
import { job, jobs, run, steps, workflow } from "../policy/workflows.ts";
import { NATIVE_SETUP_PATHS } from "./plan.ts";

const CI = workflow("ci.yml");
const AFTER = ["ci", "content", "prune-build-cache"];

test("a deliberate native or icount skip passes on a pull request and fails on main", () => {
  const script = run(steps(job(CI, "ci"))[0] ?? {});
  const lanes = Object.keys(jobs(CI)).filter((id) => !AFTER.includes(id));
  const baseline = Object.fromEntries(lanes.map((id) => [id, { result: "skipped" }]));
  baseline["plan"] = { result: "success" };
  const aggregate = (results: unknown, event: string): number =>
    Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
      env: { ...process.env, RESULTS: JSON.stringify(results), EVENT_NAME: event },
      stdout: "ignore",
    }).exitCode;
  const required = {
    ...baseline,
    native: { result: "success" },
    icount: { result: "success" },
  };
  expect(aggregate(baseline, "pull_request")).toBe(0);
  expect(aggregate(baseline, "workflow_dispatch")).toBe(0);
  expect(aggregate(baseline, "push")).toBe(1);
  expect(aggregate(baseline, "merge_group")).toBe(1);
  expect(aggregate({ ...baseline, native: { result: "success" } }, "push")).toBe(1);
  expect(aggregate({ ...baseline, icount: { result: "success" } }, "merge_group")).toBe(1);
  expect(aggregate({ ...baseline, native: { result: "failure" } }, "pull_request")).toBe(1);
  expect(aggregate({ ...baseline, icount: { result: "cancelled" } }, "pull_request")).toBe(1);
  expect(aggregate(required, "push")).toBe(0);
  expect(aggregate(required, "merge_group")).toBe(0);
  expect(aggregate({ ...required, lint: { result: "failure" } }, "push")).toBe(1);
});

test("native setup's path list covers every script that job runs", () => {
  const commands = steps(job(CI, "native")).map(run).join("\n");
  const scripts = [...commands.matchAll(/scripts\/[A-Za-z0-9_./-]+\.sh/g)].map((match) => match[0]);
  expect(scripts).toEqual([
    "scripts/test-dev-setup.sh",
    "scripts/setup-rust.sh",
    "scripts/install-zig.sh",
    "scripts/native-smoke.sh",
  ]);
  const inputs: readonly string[] = NATIVE_SETUP_PATHS;
  for (const script of scripts) expect(inputs).toContain(script);
});
