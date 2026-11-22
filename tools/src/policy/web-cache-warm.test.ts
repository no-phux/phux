import { expect, test } from "bun:test";
import { runInNewContext } from "node:vm";
import { str, table, type Table } from "../lib/value.ts";
import { classifyEvent } from "../routing/detect.ts";
import { plan, planOutputs, reused } from "../routing/plan.ts";
import { action, job, needs, run, steps, uses, withInput, workflow } from "./workflows.ts";

const WEB = ["web-vt", "web-client"];
const CI = workflow("ci.yml");
const WARM = workflow("cache-warm.yml");
const SETUP = action("setup-web-cache");
const CACHE = steps(SETUP).find((step) => uses(step).startsWith("Swatinem/rust-cache@")) ?? {};
const MAIN = {
  event_name: "workflow_dispatch",
  ref: "refs/heads/main",
  repository: "no-phux/phux",
};

function evaluate(value: unknown, context: Table): unknown {
  const expression = (str(value) ?? "")
    .replace(/^\$\{\{\s*|\s*\}\}$/g, "")
    .replaceAll("inputs.record-main-cache", 'inputs["record-main-cache"]');
  const result: unknown = runInNewContext(expression, context);
  return result;
}

test("the nightly dispatch can populate web caches even when main reuses a queue receipt", () => {
  const c = classifyEvent("workflow_dispatch", undefined);
  const outputs = planOutputs(reused(plan(c, "push")), c);
  for (const id of WEB) {
    expect(evaluate(job(CI, id)["if"], { needs: { plan: { outputs } } })).toBe(false);
    const warm = job(WARM, id);
    expect(evaluate(warm["if"], { github: MAIN })).toBe(true);
    const setup = steps(warm).find((step) => uses(step) === "./.github/actions/setup-web-cache");
    expect(
      evaluate(withInput(CACHE, "save-if"), { github: MAIN, inputs: table(setup?.["with"]) }),
    ).toBe(true);
  }
});

test("web cache dispatch saves require an explicit opt-in on this repository's main", () => {
  expect(table(table(SETUP["inputs"])["record-main-cache"])["default"]).toBe("false");
  for (const event_name of [
    "push",
    "pull_request",
    "pull_request_target",
    "merge_group",
    "schedule",
    "workflow_dispatch",
  ]) {
    for (const ref of ["refs/heads/main", "refs/heads/topic", "refs/pull/1/merge"]) {
      for (const repository of ["no-phux/phux", "contributor/phux"]) {
        for (const optIn of [undefined, "false", "true"]) {
          const allowed =
            repository === MAIN.repository &&
            ref === MAIN.ref &&
            (event_name === "push" || (event_name === "workflow_dispatch" && optIn === "true"));
          expect(
            evaluate(withInput(CACHE, "save-if"), {
              github: { event_name, ref, repository },
              inputs: { "record-main-cache": optIn },
            }),
          ).toBe(allowed);
        }
      }
    }
  }
});

test("only trusted main dispatches run browser warms; CI callers never opt in", () => {
  for (const id of WEB) {
    const warm = job(WARM, id);
    for (const github of [
      MAIN,
      { ...MAIN, ref: "refs/heads/topic" },
      { ...MAIN, repository: "contributor/phux" },
      { ...MAIN, event_name: "pull_request" },
      { ...MAIN, event_name: "merge_group" },
      { ...MAIN, event_name: "schedule" },
      { ...MAIN, event_name: "push" },
    ]) {
      expect(evaluate(warm["if"], { github })).toBe(github === MAIN);
    }
    const ciSetup = steps(job(CI, id)).find(
      (step) => uses(step) === "./.github/actions/setup-web-cache",
    );
    expect(withInput(ciSetup ?? {}, "record-main-cache")).toBeUndefined();
  }
});

test("browser warms use the qualified recipes and identical dependency cache inputs", () => {
  for (const id of WEB) {
    const warm = job(WARM, id);
    const ci = job(CI, id);
    const setup = steps(warm).find((step) => uses(step) === "./.github/actions/setup-web-cache");
    const ciSetup = steps(ci).find((step) => uses(step) === "./.github/actions/setup-web-cache");
    expect(table(setup?.["with"])).toEqual({
      ...table(ciSetup?.["with"]),
      "record-main-cache": "true",
    });
    expect(steps(warm).map(run).filter(Boolean)).toEqual(steps(ci).map(run).filter(Boolean));
    expect(warm["runs-on"]).toBe("ubuntu-24.04");
    const checkout = steps(warm).find((step) => uses(step).startsWith("actions/checkout@"));
    expect(withInput(checkout ?? {}, "persist-credentials")).toBe(false);
    expect(table(warm["permissions"])).toEqual({});
    expect(needs(job(WARM, "prune-build-cache"))).toContain(id);
  }
});
