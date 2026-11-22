import { expect, test } from "bun:test";
import { capture } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";
import { table } from "../lib/value.ts";
import { job, run, steps, workflow } from "../policy/workflows.ts";
import { Classifier } from "./classify.ts";
import { plan } from "./plan.ts";

function select(specs: string): string[] {
  return capture(
    [
      "bash",
      "-euo",
      "pipefail",
      "-c",
      'source scripts/lib/e2e-selection.sh; e2e_select; printf "%s\\n" "$e2e_filter" "${e2e_targets[@]}"',
    ],
    { cwd: ROOT, env: { ...process.env, PHUX_UNIT_TARGETS: specs } },
  )
    .trim()
    .split("\n");
}

test("a narrow archive builds exactly the e2e binaries the runner selects, plus MCP", () => {
  expect(select("phux:recording_e2e,phux-server:perf:measure,phux-client:connection")).toEqual([
    "binary_id(phux::recording_e2e) + binary_id(phux-server::perf)",
    "--test",
    "discovery",
    "--test",
    "recording_e2e",
    "--test",
    "perf",
  ]);
  expect(select("phux:service")).toEqual(select(""));
});

test("shared fixtures fail closed to the complete unit and e2e suites", () => {
  const classifier = new Classifier();
  const paths = ["crates/phux-server/tests/fixtures/upgrade-blob/0001.json"];
  const selected = plan(classifier.classify(paths), "pull_request", {
    pullRequestFast: true,
    scope: { kind: "diff", paths },
  });
  expect(selected).toMatchObject({ unitMode: "workspace", unitTargets: "", testFilterset: "" });
  expect(selected.lanes.has("tests")).toBe(true);
  expect(selected.lanes.has("e2e")).toBe(true);
});

test("CI passes the same narrow specs to the archive and e2e runner", () => {
  const ci = workflow("ci.yml");
  for (const id of ["test-build", "e2e"]) {
    const recipe = id === "test-build" ? "test-archive" : "e2e";
    const step = steps(job(ci, id)).find((candidate) => run(candidate).includes(`just ${recipe}`));
    expect(table(step?.["env"])["PHUX_UNIT_TARGETS"]).toBe(
      "${{ needs.plan.outputs.unit_targets }}",
    );
  }
});
