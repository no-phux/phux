import { expect, test } from "bun:test";
import { runInNewContext } from "node:vm";
import { capture } from "../lib/process.ts";
import { str, table, type Table } from "../lib/value.ts";
import { action, job, needs, run, steps, uses, withInput, workflow } from "./workflows.ts";

function evaluate(value: unknown, github: Table, inputs: Table = {}): unknown {
  // GitHub permits hyphens in input names; JavaScript needs brackets.
  const expression = (str(value) ?? "")
    .replace(/^\$\{\{\s*|\s*\}\}$/g, "")
    .replaceAll("inputs.record-main-cache", 'inputs["record-main-cache"]')
    .replaceAll("inputs.linux-only", 'inputs["linux-only"]');
  const result: unknown = runInNewContext(expression, { github, inputs });
  return result;
}

const LINUX = ["rust-lint", "rust-test", "rust-uniffi", "rust-icount"];
const WARM = workflow("cache-warm.yml");

test("the two scheduled warms keep Linux dispatch and Rune on their own clocks", () => {
  for (const schedule of ["10 0 * * *", "17 6 * * *"]) {
    const github = { event_name: "schedule", ref: "refs/heads/main", event: { schedule } };
    expect(evaluate(job(WARM, "rune")["if"], github)).toBe(schedule === "17 6 * * *");
    for (const id of LINUX) {
      expect(evaluate(job(WARM, id)["if"], github)).toBe(false);
    }
    expect(evaluate(job(WARM, "dispatch-linux")["if"], github)).toBe(schedule === "10 0 * * *");
  }
});

test("only an explicit main dispatch enables the action's supported dispatch-save input", () => {
  const mbx =
    steps(action("setup-rust-lane")).find((step) =>
      uses(step).startsWith("jdx/mr-boxington-action@"),
    ) ?? {};
  // NodeScriptActionHandler restores GITHUB_* from GitHub's context before
  // invoking an action. A step env override cannot change context.eventName.
  expect(table(mbx["env"])["GITHUB_EVENT_NAME"]).toBeUndefined();
  const expression = withInput(mbx, "save-on-workflow-dispatch");
  const deps =
    steps(action("setup-rust-lane")).find((step) =>
      uses(step).startsWith("Swatinem/rust-cache@"),
    ) ?? {};
  for (const event of ["push", "pull_request", "merge_group", "schedule", "workflow_dispatch"]) {
    for (const ref of ["refs/heads/main", "refs/heads/topic", "refs/pull/1/merge"]) {
      for (const opted of ["true", "false"]) {
        const mayWarm =
          ref === "refs/heads/main" && opted === "true" && event === "workflow_dispatch";
        expect(
          evaluate(expression, { event_name: event, ref }, { "record-main-cache": opted }),
        ).toBe(mayWarm);
        expect(
          evaluate(
            withInput(deps, "save-if"),
            { event_name: event, ref },
            { "record-main-cache": opted },
          ),
        ).toBe(ref === "refs/heads/main" && (event === "push" || mayWarm));
      }
    }
  }
});

test("mbx export headroom uses the real action eligibility output", () => {
  const setup = action("setup-rust-lane");
  expect(table(table(setup["outputs"])["cache-save-eligible"])["value"]).toBe(
    "${{ steps.mbx.outputs.cache-save-eligible }}",
  );
  for (const id of ["rust-lint", "rust-test", "rust-icount"]) {
    const headroom = steps(job(WARM, id)).find((step) =>
      run(step).includes("mbx-export-headroom.sh"),
    );
    expect(headroom?.["if"]).toBe(
      "steps.lane.outputs.cache-save-eligible == 'true' && steps.lane.outputs.cache-hit != 'true'",
    );
  }
});

test("the nightly dispatcher targets only this repository's main Linux warm", () => {
  const dispatch = job(WARM, "dispatch-linux");
  expect(table(dispatch["permissions"])["actions"]).toBe("write");
  const script = run(steps(dispatch)[0] ?? {});
  const output = capture(
    ["bash", "-euo", "pipefail", "-c", `gh() { printf '%s\\n' "$@"; }\n${script}`],
    {
      env: { ...process.env, GH_TOKEN: "", GH_REPO: "fixture/repo" },
    },
  );
  expect(output.trim().split("\n")).toEqual([
    "workflow",
    "run",
    "cache-warm.yml",
    "--repo",
    "fixture/repo",
    "--ref",
    "main",
    "-f",
    "linux-only=true",
  ]);
});

test("a Linux-only dispatch never enters Rune or recursively dispatches itself", () => {
  const github = { event_name: "workflow_dispatch", ref: "refs/heads/main", event: {} };
  const inputs = { "linux-only": true };
  expect(evaluate(job(WARM, "rune")["if"], github, inputs)).toBe(false);
  expect(evaluate(job(WARM, "dispatch-linux")["if"], github, inputs)).toBe(false);
  for (const id of LINUX) expect(evaluate(job(WARM, id)["if"], github, inputs)).toBe(true);
  expect(evaluate(job(WARM, "rune")["if"], github, { "linux-only": false })).toBe(true);
});

test("the Linux warm cannot replace or wait behind a pending Rune warm", () => {
  const owner = table(WARM["concurrency"]);
  const key = (event: string, schedule: string, linux: boolean) =>
    (str(owner["group"]) ?? "").replace(/\$\{\{(.*?)\}\}/g, (_, expression: string) =>
      String(
        evaluate(
          expression,
          { event_name: event, ref: "refs/heads/main", event: { schedule } },
          { "linux-only": linux },
        ),
      ),
    );
  const rune = key("schedule", "17 6 * * *", false);
  const dispatcher = key("schedule", "10 0 * * *", false);
  const linux = key("workflow_dispatch", "", true);
  expect(new Set([rune, dispatcher, linux]).size).toBe(3);
  expect(owner["cancel-in-progress"]).toBe(false);
});

test("cache eviction happens only after post-step uploads, never on save eligibility", () => {
  for (const id of LINUX) {
    const commands = steps(job(WARM, id)).map(run).join("\n");
    expect(commands).not.toContain("prune-build-cache.sh");
    expect(commands).not.toContain("--replace-prefix");
  }
  expect(needs(job(WARM, "prune-build-cache")).toSorted()).toEqual([...LINUX].toSorted());
});

test("dependency cache invalidation includes flake in shared-key, not the ignored key input", () => {
  const deps =
    steps(action("setup-rust-lane")).find((step) =>
      uses(step).startsWith("Swatinem/rust-cache@"),
    ) ?? {};
  expect(str(withInput(deps, "shared-key"))).toContain("steps.deps-env.outputs.flake");
  expect(withInput(deps, "key")).toBeUndefined();
});
