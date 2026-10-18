// Workflows use free public compute and spend the shared cache budget only
// from main.
import { expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { str, table, type Table } from "../lib/value.ts";
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

const STANDARD = new Set(["ubuntu-latest", "ubuntu-24.04", "ubuntu-24.04-arm", "xcode-27"]);
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
    for (const [, group] of body.matchAll(/^\s*group: (.+)$/gm)) {
      expect([name, group?.startsWith("mini-v1-")]).toEqual([name, true]);
    }
  }
});

test("optional scans are not scheduled", () => {
  for (const name of ["stress.yml", "mutation.yml"]) {
    expect(Object.keys(table(workflow(name)["on"]))).not.toContain("schedule");
  }
});

test("the PR janitor reclaims a closed PR's runs, then its caches", () => {
  // Caches go after the cancel, or a still-live run repopulates the scope.
  const janitor = workflow("pr-janitor.yml");
  expect(table(table(janitor["on"])["pull_request"])["types"]).toEqual(["closed"]);
  expect(table(janitor["permissions"])["actions"]).toBe("write");
  expect(needs(job(janitor, "caches"))).toEqual(["cancel"]);
});

test("pull request lanes never write to the shared cache budget", () => {
  // The 10 GB Actions cache is LRU across every ref, and a PR entry is
  // restorable only from that PR: any PR write evicts the warm main entries.
  const pullRequest = workflowNames()
    .map(workflow)
    .filter((document) => "pull_request" in table(document["on"]));
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
  expect(caches).toBeGreaterThan(2);
});

test("only main prunes the build cache", () => {
  const prune = job(workflow("ci.yml"), "prune-build-cache");
  expect(str(prune["if"])).toContain(`github.event_name == 'push' && ${MAIN}`);
  expect(table(prune["permissions"])["actions"]).toBe("write");
});

test("the failed-lane export script exports only into its own directory", () => {
  const script = repoPath("scripts/ci/mbx-export-failed-lane.sh");
  const scratch = mkdtempSync(join(tmpdir(), "phux-failed-lane-"));
  const bash = (dest: string, env: Record<string, string>) =>
    Bun.spawnSync(["bash", script, join(scratch, dest)], { env, stdout: "pipe", stderr: "pipe" });
  try {
    const output = join(scratch, "github-output");
    expect(bash("elsewhere", { PATH: "/usr/bin:/bin" }).exitCode).toBe(2);
    expect(
      bash("phux-mbx-objects", { PATH: "/usr/bin:/bin", GITHUB_OUTPUT: output }).exitCode,
    ).toBe(0);
    expect(existsSync(output) ? readFileSync(output, "utf8") : "").not.toContain("exported=true");
    // mbx cache export --group GROUP --format directory DEST
    mkdirSync(join(scratch, "bin"));
    writeFileSync(join(scratch, "bin/mbx"), '#!/bin/sh\nmkdir -p "$7"\nexit 0\n', { mode: 0o755 });
    const exported = bash("phux-mbx-objects", {
      PATH: `${join(scratch, "bin")}:/usr/bin:/bin`,
      MBX_CACHE_EXPORT_GROUP: "group-1",
      GITHUB_OUTPUT: output,
    });
    expect(exported.exitCode, exported.stderr.toString()).toBe(0);
    expect(readFileSync(output, "utf8")).toContain("exported=true");
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});
