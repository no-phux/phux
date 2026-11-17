import { expect, test } from "bun:test";
import { Classifier } from "./classify.ts";
import { LINUX_QUALIFY, plan, planOutputs, reused, triggerFor } from "./plan.ts";
import { action, job, needs, run, steps, uses, withInput, workflow } from "../policy/workflows.ts";
import { str, table } from "../lib/value.ts";

const classifier = new Classifier();

test.each([
  "clients/rune/src/main.rs",
  "crates/phux-protocol/src/lib.rs",
  "Cargo.toml",
  ".cargo/config.toml",
])("%s has partial PR coverage and full queue/main Linux qualification", (path) => {
  const c = classifier.classify([path]);
  const options = { queueVerified: true, scope: { kind: "diff" as const, paths: [path] } };
  const pr = plan(c, "pull_request", options);
  expect(pr.lanes.has("rune")).toBe(true);
  expect(pr.runeQualify).toBe(false);
  for (const token of LINUX_QUALIFY) expect(pr.coverage.split(",")).not.toContain(token);
  for (const event of ["push", "merge_group"]) {
    const full = plan(c, triggerFor(event), options);
    expect(full.coverage).toBe(`${pr.coverage},linux-aarch64,packaging`);
    expect(planOutputs(full, c)).toMatchObject({ rune: true, rune_qualify: true });
    expect(full).toMatchObject({ unitMode: "workspace", testFilterset: "" });
    const output = planOutputs(reused(full), c);
    expect(output).toMatchObject({ native: true, icount: true, rune: false, rune_qualify: false });
  }
});

test("Linux mode is an explicit option, never the legacy queue boolean", () => {
  const paths = ["clients/rune/src/main.rs"];
  const c = classifier.classify(paths);
  const scope = { kind: "diff" as const, paths };
  const pr = plan(c, "pull_request", { scope, queueVerified: true });
  const dispatch = plan(c, "pull_request", { scope, queueVerified: true, runeQualify: true });
  expect(dispatch.coverage).toBe(`${pr.coverage},linux-aarch64,packaging`);
  expect([...dispatch.lanes]).toEqual([...pr.lanes]);
  for (const legacy of [true, false]) {
    expect(plan(c, "pull_request", legacy).runeQualify).toBe(true);
    expect(plan(c, "push", legacy).runeQualify).toBe(true);
  }
  const docs = plan(classifier.classify(["docs/development.md"]), "push");
  expect(docs.runeQualify).toBe(false);
  for (const token of LINUX_QUALIFY) expect(docs.coverage.split(",")).not.toContain(token);
});

test("Linux qualification uses the existing host action and defaults on for tags and nightly", () => {
  const build = workflow("rune-build.yml");
  const inputs = table(table(table(build["on"])["workflow_call"])["inputs"]);
  expect(table(inputs["qualify"])).toMatchObject({ type: "boolean", default: true });
  const ci = workflow("ci.yml");
  expect(withInput(job(ci, "rune"), "qualify")).toBe(
    "${{ needs.plan.outputs.rune_qualify == 'true' }}",
  );
  expect(needs(job(ci, "ci"))).toContain("queue-apple");
  for (const id of ["linux-x86_64", "linux-aarch64"]) {
    const host = steps(job(build, id)).find((step) => uses(step) === "./.github/actions/rune-host");
    expect(withInput(host ?? {}, "qualify")).toBe("${{ inputs.qualify }}");
  }
  expect(str(job(build, "linux-aarch64")["if"])).toContain("inputs.qualify");
  for (const [file, id] of [
    ["rune-client.yml", "build"],
    ["cache-warm.yml", "rune"],
    ["rune-macos.yml", "build"],
  ]) {
    expect(withInput(job(workflow(file ?? ""), id ?? ""), "qualify")).toBeUndefined();
  }
  const host = steps(action("rune-host"));
  expect(host.map(run).join("\n")).toContain("clients/rune/scripts/check.sh");
  const packaged = host.filter((step) =>
    ["Package", "Launch the packaged app against a same-source server"].includes(
      str(step["name"]) ?? "",
    ),
  );
  expect(packaged).toHaveLength(2);
  for (const step of packaged) expect(step["if"]).toBe("inputs.qualify == 'true'");
});
