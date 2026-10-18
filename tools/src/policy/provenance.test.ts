// Artifact workflows build the commit their run names, never a moving branch.
//
// phux-mobile resolves a workflow run by its headSha and then fails closed
// when the artifact's provenance names another commit (phux-dm8ik). A
// checkout of `github.ref` builds whatever the branch is when the job STARTS,
// so a queued push run stamped a later main's provenance under an earlier
// headSha.
import { describe, expect, test } from "bun:test";
import { str, table } from "../lib/value.ts";
import {
  jobs,
  run,
  steps,
  uses,
  withInput,
  workflow,
  workflowNames,
  workflowText,
} from "./workflows.ts";

const MOBILE_ARTIFACT_WORKFLOWS = ["ffi-xcframework.yml", "ffi-android.yml"];
const BUILD_REF = "${{ inputs.tag || inputs.commit || github.sha }}";
const MOVING_REF = /github\.(?:ref|head_ref|ref_name)\b/;

test("no checkout follows the triggering branch", () => {
  // A checkout `ref:` that names the event's branch resolves at job start.
  for (const name of workflowNames()) {
    for (const [, value] of workflowText(name).matchAll(/^\s+ref: (.+)$/gm)) {
      expect([name, value, MOVING_REF.test(value ?? "")]).toEqual([name, value, false]);
    }
  }
});

describe.each(MOBILE_ARTIFACT_WORKFLOWS)("%s", (name) => {
  const document = workflow(name);
  const body = workflowText(name);
  const all = Object.values(jobs(document)).flatMap(steps);

  test("builds and verifies the run commit", () => {
    const checkouts = all.filter((step) => uses(step).startsWith("actions/checkout@"));
    expect(checkouts.map((step) => withInput(step, "ref"))).toContain(BUILD_REF);
    expect(body).toContain('test "$(git rev-parse HEAD)" = "$expected"');
    const verify = all.find((step) => step["name"] === "Verify provenance names the build commit");
    expect(verify).toBeDefined();
    expect(run(verify ?? {})).toMatch(/grep -qx "phux[-_]rev[ =]\$BUILD_SHA"/);
  });

  test("dispatch takes an explicit commit", () => {
    const dispatch = table(table(document["on"])["workflow_dispatch"]);
    expect(Object.keys(table(dispatch["inputs"]))).toContain("commit");
    // A commit other than the run's headSha must not reuse the canonical
    // artifact names a headSha lookup would download.
    expect(body).toContain('suffix="-$expected"');
    const artifacts = all
      .map((step) => str(withInput(step, "name")))
      // Match the prefix only: the suffix expression itself contains spaces.
      .filter((artifact): artifact is string => /^Phux\S*FFI-/.test(artifact ?? ""));
    expect(artifacts.length).toBeGreaterThan(0);
    for (const artifact of artifacts) expect(artifact).toEndWith("${{ env.ARTIFACT_SUFFIX }}");
  });

  test("runs are grouped per commit and never cancelled", () => {
    // A ref-keyed group replaces a pending run, so its commit never gets artifacts.
    const concurrency = table(document["concurrency"]);
    const group = str(concurrency["group"]) ?? "";
    expect(group).toContain(BUILD_REF);
    expect(group).not.toMatch(MOVING_REF);
    expect(concurrency["cancel-in-progress"]).toBe(false);
  });
});
