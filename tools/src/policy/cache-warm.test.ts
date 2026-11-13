import { expect, test } from "bun:test";
import { runInNewContext } from "node:vm";
import { str, table, type Table } from "../lib/value.ts";
import { action, job, needs, run, steps, uses, withInput, workflow } from "./workflows.ts";

function evaluate(value: unknown, github: Table, inputs: Table = {}): unknown {
  // GitHub permits a hyphen in this input name; JavaScript needs brackets.
  const expression = (str(value) ?? "")
    .replace(/^\$\{\{\s*|\s*\}\}$/g, "")
    .replaceAll("inputs.record-main-cache", 'inputs["record-main-cache"]');
  const result: unknown = runInNewContext(expression, { github, inputs });
  return result;
}

const LINUX = ["rust-lint", "rust-test", "rust-uniffi", "rust-icount"];
const WARM = workflow("cache-warm.yml");

test("the two scheduled warms keep Linux and Rune on their own clocks", () => {
  for (const schedule of ["10 0 * * *", "17 6 * * *"]) {
    const github = { event_name: "schedule", ref: "refs/heads/main", event: { schedule } };
    expect(evaluate(job(WARM, "rune")["if"], github)).toBe(schedule === "17 6 * * *");
    for (const id of LINUX) {
      expect(evaluate(job(WARM, id)["if"], github)).toBe(schedule === "10 0 * * *");
    }
  }
});

test("only explicit main schedule/dispatch warmups can report a saving push", () => {
  const mbx =
    steps(action("setup-rust-lane")).find((step) =>
      uses(step).startsWith("jdx/mr-boxington-action@"),
    ) ?? {};
  const expression = table(mbx["env"])["GITHUB_EVENT_NAME"];
  for (const event of ["push", "pull_request", "merge_group", "schedule", "workflow_dispatch"]) {
    for (const ref of ["refs/heads/main", "refs/heads/topic", "refs/pull/1/merge"]) {
      for (const opted of ["true", "false"]) {
        const mayWarm =
          ref === "refs/heads/main" &&
          opted === "true" &&
          ["schedule", "workflow_dispatch"].includes(event);
        expect(
          evaluate(expression, { event_name: event, ref }, { "record-main-cache": opted }),
        ).toBe(mayWarm ? "push" : event);
      }
    }
  }
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
