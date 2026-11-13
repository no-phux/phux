// Workflows use free public compute and spend the shared cache budget only
// from main.
import { expect, test } from "bun:test";
import { list, str, table, type Table } from "../lib/value.ts";
import {
  action,
  job,
  jobs,
  needs,
  steps,
  uses,
  withInput,
  workflow,
  workflowNames,
  workflowText,
} from "./workflows.ts";

const STANDARD = new Set([
  "ubuntu-latest",
  "ubuntu-24.04",
  "ubuntu-24.04-arm",
  "windows-2022",
  "windows-2025",
  "xcode-27",
  "macos-26",
]);
const MAIN = "github.ref == 'refs/heads/main'";
const CACHE_WRITERS = new Set([
  "actions/cache/save",
  "Swatinem/rust-cache",
  "jdx/mr-boxington-action",
]);

test("all runner selectors are standard public capacity", () => {
  for (const name of workflowNames()) {
    const body = workflowText(name);
    for (const [, selector] of body.matchAll(/^\s*(?:runs-on:|- os:) (.+)$/gm)) {
      const allowed = STANDARD.has(selector ?? "") || selector === "${{ matrix.os }}";
      expect([name, selector, allowed]).toEqual([name, selector, true]);
    }
  }
});

test("Rune qualifies and releases on macOS and Linux", () => {
  const rune = job(workflow("rune-build.yml"), "build");
  const matrix = table(table(rune["strategy"])["matrix"]);
  const runners = list(matrix["include"]).map((entry) => table(entry)["os"]);
  expect(runners).toContain("macos-26");
  expect(runners).toContain("ubuntu-24.04");
});

test("closing a pull request cancels its in-flight ci run", () => {
  // `closed` joins the PR's latest-wins group and then runs nothing.
  const ci = workflow("ci.yml");
  expect(table(table(ci["on"])["pull_request"])["types"]).toContain("closed");
  expect(table(ci["concurrency"])["cancel-in-progress"]).toBe(true);
  for (const [id, lane] of Object.entries(jobs(ci))) {
    const gate = str(lane["if"]) ?? "";
    const reaches = needs(lane).length > 0 && !gate.includes("always()");
    expect([
      id,
      reaches ||
        gate.includes("github.event.action != 'closed'") ||
        gate.includes("event_name == 'push'"),
    ]).toEqual([id, true]);
  }
});

test("pull request lanes never write to the shared cache budget", () => {
  // The 10 GB Actions cache is LRU across every ref, and a PR entry is
  // restorable only from that PR: any PR write evicts the warm main entries.
  const triggered = workflowNames()
    .map(workflow)
    .filter((document) => "pull_request" in table(document["on"]));
  // A called workflow runs under its caller's event, so it is a PR lane too.
  const called = triggered
    .flatMap((document) => Object.values(jobs(document)).map(uses))
    .filter((ref) => ref.startsWith("./.github/workflows/"))
    .map((ref) => workflow(ref.replace("./.github/workflows/", "")));
  const pullRequest = [...triggered, ...called];
  const all: Table[] = [
    ...steps(action("setup-rust-lane")),
    ...pullRequest.flatMap((document) => Object.values(jobs(document)).flatMap(steps)),
  ];
  let caches = 0;
  for (const step of all) {
    const name = uses(step).split("@")[0] ?? "";
    if (name === "actions/cache/save") expect(str(step["if"])).toContain(MAIN);
    if (name === "Swatinem/rust-cache") expect(str(withInput(step, "save-if"))).toContain(MAIN);
    // The only extra save mode is an explicit main dispatch opt-in. The
    // runner supplies the real source event; PRs cannot enter this mode.
    const inputs = Object.keys(table(step["with"]));
    if (name === "jdx/mr-boxington-action") {
      expect(inputs.filter((key) => key.startsWith("save-on"))).toEqual([
        "save-on-workflow-dispatch",
      ]);
      expect(withInput(step, "save-on-workflow-dispatch")).toBe(
        "${{ inputs.record-main-cache == 'true' && github.ref == 'refs/heads/main' && github.event_name == 'workflow_dispatch' }}",
      );
    }
    // actions/cache saves on every event.
    expect(name).not.toBe("actions/cache");
    if (CACHE_WRITERS.has(name)) caches++;
  }
  expect(caches).toBeGreaterThanOrEqual(2);
});

test("only main prunes the build cache", () => {
  const prune = job(workflow("ci.yml"), "prune-build-cache");
  expect(str(prune["if"])).toContain(`github.event_name == 'push' && ${MAIN}`);
  expect(table(prune["permissions"])["actions"]).toBe("write");
  const warm = job(workflow("cache-warm.yml"), "prune-build-cache");
  const gate = str(warm["if"]) ?? "";
  expect(gate).toContain(MAIN);
  expect(gate).toContain("github.event_name == 'workflow_dispatch'");
  expect(table(warm["permissions"])["actions"]).toBe("write");
});

test("the mbx warm stays on Linux and only main dispatches opt into saves", () => {
  const warm = workflow("cache-warm.yml");
  const rune = str(job(warm, "rune")["if"]) ?? "";
  expect(rune).toContain(MAIN);
  expect(rune).toContain("17 6 * * *");
  expect(rune).not.toContain("10 0 * * *");
  for (const id of ["rust-lint", "rust-test", "rust-uniffi", "rust-icount"]) {
    const lane = job(warm, id);
    expect(str(lane["runs-on"]) ?? "").toMatch(/^ubuntu-/);
    const gate = str(lane["if"]) ?? "";
    expect(gate).toBe("github.event_name == 'workflow_dispatch'");
    expect(gate).not.toContain("17 6 * * *");
  }
  const setup = action("setup-rust-lane");
  expect(str(table(table(setup["inputs"])["record-main-cache"])["default"])).toBe("false");
  const mbx = steps(setup).find((step) => uses(step).startsWith("jdx/mr-boxington-action"));
  expect(table(mbx?.["env"])["GITHUB_EVENT_NAME"]).toBeUndefined();
  const saveDispatch = str(withInput(mbx ?? {}, "save-on-workflow-dispatch")) ?? "";
  expect(saveDispatch).toContain("inputs.record-main-cache == 'true'");
  expect(saveDispatch).toContain(MAIN);
  expect(saveDispatch).toContain("github.event_name == 'workflow_dispatch'");
  const deps = steps(setup).find((step) => uses(step).startsWith("Swatinem/rust-cache"));
  const saveIf = str(withInput(deps ?? {}, "save-if")) ?? "";
  expect(saveIf).toContain(MAIN);
  expect(saveIf).toContain("github.event_name == 'workflow_dispatch'");
  expect(saveIf).toContain("inputs.record-main-cache == 'true'");
  const opted = workflowNames().flatMap((name) =>
    Object.values(jobs(workflow(name)))
      .flatMap(steps)
      .filter((step) => uses(step) === "./.github/actions/setup-rust-lane")
      .filter((step) => str(withInput(step, "record-main-cache")) === "true")
      .map(() => name),
  );
  expect(opted).toEqual(["cache-warm.yml", "cache-warm.yml", "cache-warm.yml", "cache-warm.yml"]);
});
