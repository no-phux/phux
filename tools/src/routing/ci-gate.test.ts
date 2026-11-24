// The pull-request exception for native setup and rust icount, and the
// scripts that job actually runs. workflows.test.ts keeps the shared
// success-or-skip cases; this file is the event split.
import { expect, test } from "bun:test";
import { repoPath } from "../lib/repo.ts";
import { job, jobs, run, steps, workflow } from "../policy/workflows.ts";
import { LANES, NATIVE_SETUP_PATHS, plan, planOutputs, reused, triggerFor } from "./plan.ts";
import { classifyDetection } from "./detect.ts";

const CI = workflow("ci.yml");
const AFTER = ["ci", "content", "prune-build-cache"];
const script = run(
  steps(job(CI, "ci")).find((step) => run(step).includes("scripts/ci/ci-aggregate.sh")) ?? {},
);
const lanes = Object.keys(jobs(CI)).filter((id) => !AFTER.includes(id));
const baseline = {
  ...Object.fromEntries(lanes.map((id) => [id, { result: "skipped" }])),
  plan: {
    result: "success",
    outputs: Object.fromEntries(
      [...LANES, "rune_qualify", "docs_catalog", "release_qualify"].map((key) => [key, "false"]),
    ),
  },
  "queue-apple": { result: "success" },
};
const aggregate = (results: unknown, event: string): number =>
  Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
    cwd: repoPath("."),
    env: { ...process.env, RESULTS: JSON.stringify(results), EVENT_NAME: event },
    stdout: "ignore",
  }).exitCode;

test("a deliberate native or icount skip passes on a pull request and fails on main", () => {
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

test.each(["push", "merge_group"])("%s rejects either mandatory lane's non-success", (event) => {
  for (const id of ["native", "icount"]) {
    for (const result of ["failure", "cancelled", "skipped"]) {
      const results = {
        ...baseline,
        native: { result: "success" },
        icount: { result: "success" },
        "web-vt": { result: "success" },
        "web-client": { result: "success" },
        [id]: { result },
      };
      expect([id, result, aggregate(results, event)]).toEqual([id, result, 1]);
    }
  }
});

test.each(["push", "merge_group"])(
  "%s receipt reuse keeps all mandatory native proofs",
  (event) => {
    const c = classifyDetection({ kind: "release-metadata" });
    const p = reused(plan(c, triggerFor(event), { scope: { kind: "none" } }));
    const output = planOutputs(p, c);
    expect(output).toMatchObject({ native: true, icount: true, web: false, rune: false });
    const results = {
      ...baseline,
      native: { result: output["native"] ? "success" : "skipped" },
      icount: { result: output["icount"] ? "success" : "skipped" },
      "queue-apple": { result: event === "merge_group" ? "success" : "skipped" },
    };
    expect(aggregate(results, event)).toBe(0);
    for (const lane of event === "merge_group"
      ? ["native", "icount", "queue-apple"]
      : ["native", "icount"]) {
      expect(aggregate({ ...results, [lane]: { result: "skipped" } }, event)).toBe(1);
    }
  },
);

test.each(["pull_request", "push", "merge_group"])(
  "%s checks both browser jobs even when native and icount pass",
  (event) => {
    for (const id of ["web-vt", "web-client"]) {
      for (const result of ["success", "failure", "cancelled", "skipped"]) {
        const results = {
          ...baseline,
          plan: { result: "success", outputs: { ...baseline.plan.outputs, web: "true" } },
          native: { result: "success" },
          icount: { result: "success" },
          "web-vt": { result: "success" },
          "web-client": { result: "success" },
          [id]: { result },
        };
        const expected = result === "success" ? 0 : 1;
        expect([id, result, aggregate(results, event)]).toEqual([id, result, expected]);
      }
    }
  },
);

test("a skipped docs catalog passes on main and a failure does not", () => {
  const ready = {
    ...baseline,
    native: { result: "success" },
    icount: { result: "success" },
  };
  expect(aggregate({ ...ready, "docs-catalog": { result: "skipped" } }, "push")).toBe(0);
  expect(aggregate({ ...ready, "docs-catalog": { result: "skipped" } }, "merge_group")).toBe(0);
  expect(aggregate({ ...ready, "docs-catalog": { result: "failure" } }, "pull_request")).toBe(1);
  expect(aggregate({ ...ready, "docs-catalog": { result: "failure" } }, "push")).toBe(1);
});

test("a draft's skipped plan fails ci, so it cannot satisfy a readied PR", () => {
  // GitHub counts a skipped required check as passing.
  expect(String(job(CI, "ci")["if"])).not.toContain("draft");
  const draft = { ...baseline, plan: { result: "skipped" }, "queue-apple": { result: "skipped" } };
  expect(aggregate(draft, "pull_request")).toBe(1);
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
