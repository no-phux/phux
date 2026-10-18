// Artifact workflows build the commit their run names, never a moving branch.
// phux-mobile resolves a run by its headSha and fails closed when the
// artifact's provenance names another commit; a `github.ref` checkout builds
// whatever the branch is when the job starts.
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

const BUILD_REF = "${{ inputs.tag || inputs.commit || github.sha }}";
const MOVING_REF = /github\.(?:ref|head_ref|ref_name)\b/;

test("no checkout follows the triggering branch", () => {
  for (const name of workflowNames()) {
    for (const [, value] of workflowText(name).matchAll(/^\s+ref: (.+)$/gm)) {
      expect([name, value, MOVING_REF.test(value ?? "")]).toEqual([name, value, false]);
    }
  }
});

describe.each(["ffi-xcframework.yml", "ffi-android.yml"])("%s", (name) => {
  const document = workflow(name);
  const body = workflowText(name);
  const all = Object.values(jobs(document)).flatMap(steps);

  test("builds and verifies the run commit", () => {
    const checkouts = all.filter((step) => uses(step).startsWith("actions/checkout@"));
    expect(checkouts.map((step) => withInput(step, "ref"))).toContain(BUILD_REF);
    expect(body).toContain('test "$(git rev-parse HEAD)" = "$expected"');
    const verify = all.find((step) => step["name"] === "Verify provenance names the build commit");
    expect(run(verify ?? {})).toMatch(/grep -qx "phux[-_]rev[ =]\$BUILD_SHA"/);
  });

  test("a dispatched commit gets suffixed artifact names", () => {
    // Another commit must not reuse the names a headSha lookup downloads.
    const dispatch = table(table(document["on"])["workflow_dispatch"]);
    expect(Object.keys(table(dispatch["inputs"]))).toContain("commit");
    expect(body).toContain('suffix="-$expected"');
    const artifacts = all
      .map((step) => str(withInput(step, "name")) ?? "")
      .filter((artifact) => /^Phux\S*FFI-/.test(artifact));
    expect(artifacts.length).toBeGreaterThan(0);
    for (const artifact of artifacts) expect(artifact).toEndWith("${{ env.ARTIFACT_SUFFIX }}");
  });

  test("runs are grouped per commit and never cancelled", () => {
    // A ref-keyed group replaces a pending run, so its commit never gets artifacts.
    const concurrency = table(document["concurrency"]);
    expect(str(concurrency["group"])).toContain(BUILD_REF);
    expect(str(concurrency["group"])).not.toMatch(MOVING_REF);
    expect(concurrency["cancel-in-progress"]).toBe(false);
  });
});
