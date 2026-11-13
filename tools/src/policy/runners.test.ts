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
  // Literal true cancels every event. The only expression is
  // `github.event_name != 'push'`: false for a push, true for a pull
  // request, a merge group, and a dispatch. Ordinary main pushes share
  // `main-push`; a release-please push keeps its own sha.
  const ci = workflow("ci.yml");
  const concurrency = table(ci["concurrency"]);
  expect(table(table(ci["on"])["pull_request"])["types"]).toContain("closed");
  const cancel = concurrency["cancel-in-progress"];
  for (const event of ["pull_request", "merge_group", "workflow_dispatch"]) {
    expect([event, cancelsInFlight(cancel, event)]).toEqual([event, true]);
  }
  if (cancel !== true) expect(cancelsInFlight(cancel, "push")).toBe(false);
  expect(cancelsInFlight(true, "pull_request")).toBe(true);
  expect(cancelsInFlight(true, "push")).toBe(true);
  expect(cancelsInFlight(false, "pull_request")).toBe(false);
  expect(cancelsInFlight(undefined, "pull_request")).toBe(false);
  expect(cancelsInFlight("${{ github.event_name != 'push' }}", "pull_request")).toBe(true);
  expect(cancelsInFlight("${{ github.event_name != 'push' }}", "merge_group")).toBe(true);
  expect(cancelsInFlight("${{ github.event_name != 'push' }}", "push")).toBe(false);
  expect(cancelsInFlight("${{ github.event_name == 'pull_request' }}", "pull_request")).toBe(false);
  expect(cancelsInFlight("${{ github.event_name == 'push' }}", "pull_request")).toBe(false);
  expect(cancelsInFlight("${{ true }}", "pull_request")).toBe(false);
  expect(cancelsInFlight("${{ github.event_name != 'main' }}", "pull_request")).toBe(false);
  expect(str(concurrency["group"])).toBe(
    "ci-${{ github.event_name == 'push' && (github.actor == 'phux-release-bot[bot]' && github.sha || 'main-push') || github.event.pull_request.number || github.ref }}",
  );
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
    // mbx saves only from default-branch pushes unless an input opts in.
    const inputs = Object.keys(table(step["with"]));
    if (name === "jdx/mr-boxington-action")
      expect(inputs.filter((key) => key.startsWith("save-on"))).toEqual([]);
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
});

/** Literal `true`, or `${{ github.event_name != 'push' }}` (either quote, any space). */
function cancelsInFlight(value: unknown, event: string): boolean {
  if (value === true) return true;
  if (typeof value !== "string") return false;
  const body = value
    .trim()
    .replace(/^\$\{\{\s*/, "")
    .replace(/\s*\}\}$/, "")
    .replace(/\s+/g, "");
  if (body === "github.event_name!='push'" || body === 'github.event_name!="push"') {
    return event !== "push";
  }
  return false;
}
