// Workflows use free public compute, spend the shared cache budget only from
// main, and keep the build-once lane shape.
import { describe, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { str, table } from "../lib/value.ts";
import {
  actionText,
  job,
  jobs,
  needs,
  run,
  steps,
  uses,
  withInput,
  workflow,
  workflowNames,
  workflowText,
} from "./workflows.ts";

const STANDARD = new Set(["ubuntu-latest", "ubuntu-24.04", "ubuntu-24.04-arm", "xcode-27"]);

test("all runner selectors are standard public capacity", () => {
  for (const name of workflowNames()) {
    const body = workflowText(name);
    expect([name, body.includes("blacksmith-")]).toEqual([name, false]);
    expect([name, body.includes("self-hosted,")]).toEqual([name, false]);
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

test("the mutation workflow keeps Rust without the archived Zig pilot", () => {
  const mutation = workflowText("mutation.yml");
  expect(mutation).toContain("run: bun tools/main.ts mutation-run");
  expect(mutation).not.toContain("scripts/mutation/zig.sh");
  expect(mutation).not.toContain("test_zig.py");
  expect(mutation).not.toContain("inputs.language");
});

test("the PR janitor reclaims concurrency and cache budget", () => {
  // A closed PR holds queue slots and cache bytes until something frees them.
  // The free-runner cutover retired cancellation because runner minutes had
  // stopped costing money. Concurrency and the 10 GB cache cap are still
  // finite and still shared, so the janitor reclaims both on `closed`.
  const janitor = workflow("pr-janitor.yml");
  const body = workflowText("pr-janitor.yml");
  expect(table(table(janitor["on"])["pull_request"])["types"]).toEqual(["closed"]);
  expect(table(janitor["permissions"])["actions"]).toBe("write");
  expect(body).toContain("/actions/runs/${id}/cancel");
  expect(body).toContain("/actions/caches/${id}");
  // Caches go after the cancel, or a still-live run repopulates the scope.
  expect(needs(job(janitor, "caches"))).toEqual(["cancel"]);
});

test("pull request lanes never write to the shared cache budget", () => {
  // Actions cache is 10 GB per repository, LRU across every ref. A
  // `refs/pull/N/merge` entry is restorable only from that PR, so any PR
  // write is unreachable bytes that evict the warm `main` entries all lanes
  // restore from. The mbx action saves only from pushes to the default
  // branch unless an input opts other events in, so the lane must name none
  // of those inputs.
  const lane = actionText("setup-rust-lane");
  expect(lane).toContain("uses: jdx/mr-boxington-action@");
  for (const optIn of [
    "save-on-pull-request",
    "save-on-workflow-dispatch",
    "save-on-protected-branch",
  ]) {
    expect(lane).not.toContain(optIn);
  }
  for (const name of ["ci.yml", "stress.yml"]) {
    expect(workflowText(name)).not.toContain("mr-boxington-action");
  }
  // The dependency-only cache of the test lanes follows the same rule.
  expect(lane).toContain("uses: Swatinem/rust-cache@");
  expect(lane).toContain(
    "save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}",
  );
});

test("main prunes superseded build cache entries", () => {
  // Keep the newest large archive. Delete sccache's per-object shards.
  //
  // Restores take only the newest entry for a prefix. sccache's GitHub
  // backend writes one small object per compilation; those stay LRU-hot and
  // evict the test-lane dependency archives. The xcframework dep archive did
  // the same at ~1 GB a copy.
  const prune = job(workflow("ci.yml"), "prune-build-cache");
  expect(str(prune["if"])).toContain(
    "github.event_name == 'push' && github.ref == 'refs/heads/main'",
  );
  expect(needs(prune)).toEqual(["check", "test-build", "uniffi", "perf-icount"]);
  expect(table(prune["permissions"])["actions"]).toBe("write");
  const script = steps(prune).map(run).join("\n");
  for (const line of [
    "keep_newest check linux-arm64-mbx-phux-check-",
    "keep_newest test linux-arm64-mbx-phux-test-",
    "keep_newest uniffi-deps v0-rust-phux-uniffi-Linux-",
    // The test build moved to mbx and the run jobs restore no cache.
    "drop_all test-deps v0-rust-phux-test-Linux-",
    "drop_all test-integration-deps v0-rust-phux-test-integration-Linux-",
    "drop_all e2e-deps v0-rust-phux-e2e-Linux-",
    "keep_newest icount linux-x64-mbx-phux-icount-",
    "drop_all sccache sccache",
    "drop_all xcframework-deps v0-rust-mini-v1-ffi-xcframework-",
    "keep_newest desktop-release v0-rust-mini-v1-desktop-release-",
    "keep_newest xcframework-zig mini-v1-ffi-xcframework-zig-",
    "tail -n +2",
  ]) {
    expect(script).toContain(line);
  }
  const ffi = workflowText("ffi-xcframework.yml");
  expect(ffi).not.toContain("shared-key: mini-v1-ffi-xcframework");
  expect(ffi).toContain("mini-v1-ffi-xcframework-zig-");
});

test("the Rust lane cache keeps action results and skips repeat exports", () => {
  // The test lane's 8 GiB collection deleted the store it had just restored.
  //
  // Run 38442503688 restored ~19 GiB logical, evicted every action result,
  // compiled with 0 hits, then spent ~11 minutes exporting that cold rebuild
  // because the generated key included the commit. 48 GiB kept those hits
  // and then outgrew the export copy (run 38505694112).
  const lane = actionText("setup-rust-lane");
  expect(lane).not.toContain("mbx gc --max-size 8GiB");
  expect(lane).not.toContain("mbx gc --max-size 48GiB");
  expect(lane).toContain("mbx gc --max-size 40GiB");
  expect(lane).toContain("cache-key: ${{ steps.mbx-env.outputs.cache_key }}");
  expect(lane).toContain("restore-keys: ${{ steps.mbx-env.outputs.restore_key }}");
  expect(lane).not.toContain("github.sha");
  expect(lane).toContain("date -u +%Y%m%d");
  expect(lane).toContain("linux-arm64");
  expect(lane).toContain("linux-x64");
});

/** Steps of `ci.yml` jobs that use setup-rust-lane, by job name. */
function rustLanes(): [string, Record<string, unknown>][] {
  return Object.entries(jobs(workflow("ci.yml"))).flatMap(([name, lane]) =>
    steps(lane)
      .filter((step) => uses(step) === "./.github/actions/setup-rust-lane")
      .map((step): [string, Record<string, unknown>] => [name, table(step["with"])]),
  );
}

test("test lanes compile once and run from the archive", () => {
  // The test profile compiles once; the run jobs only execute.
  //
  // Run 38982580303 compiled the workspace crates three times in parallel
  // lanes (4m 52s, 3m 38s, 2m 21s); one archive build serves the unit halves
  // and e2e, which restore no build cache. Without debuginfo or incremental
  // state the test build's mbx store is about 0.7 GB, not the 6.1 GB that
  // run 38503810705 spent 6m 28s unpacking.
  const lane = actionText("setup-rust-lane");
  expect(lane).toContain("default: mbx");
  expect(lane).toContain("CARGO_INCREMENTAL=0");
  expect(lane).toContain("CARGO_PROFILE_DEV_DEBUG=0");
  expect(lane).toContain("shared-key: phux-${{ inputs.cache-lane }}");
  expect(lane).toContain('cache-on-failure: "true"');

  const ci = workflow("ci.yml");
  const text = workflowText("ci.yml");
  // uniffi keeps the dependency cache; the three run jobs restore none.
  const caches = rustLanes().map(([, inputs]) => inputs["build-cache"]);
  expect(caches.filter((cache) => cache === "deps")).toHaveLength(1);
  expect(caches.filter((cache) => cache === "none")).toHaveLength(3);

  const build = job(ci, "test-build");
  const setup = steps(build).find((step) => uses(step) === "./.github/actions/setup-rust-lane");
  expect(withInput(setup ?? {}, "cache-lane")).toBe("test");
  expect(withInput(setup ?? {}, "build-cache")).toBeUndefined();
  expect(table(build["env"])).toMatchObject({
    CARGO_INCREMENTAL: "0",
    CARGO_PROFILE_DEV_DEBUG: "0",
  });
  expect(steps(build).map(run).join("\n")).toContain("just test-archive");
  const uploads = steps(build).filter((step) => uses(step).startsWith("actions/upload-artifact@"));
  expect(uploads.length).toBeGreaterThan(0);
  for (const upload of uploads) expect(withInput(upload, "compression-level")).toBe(0);

  const runJobs = Object.values(jobs(ci)).filter(
    (lane) => needs(lane).join(",") === "changes,test-build",
  );
  expect(runJobs).toHaveLength(3);
  const downloads = Object.values(jobs(ci))
    .flatMap(steps)
    .filter((step) => uses(step).startsWith("actions/download-artifact@"));
  expect(downloads).toHaveLength(3);
  expect(text.split("PHUX_TEST_ARCHIVE: ${{ runner.temp }}/phux-tests.tar.zst").length - 1).toBe(4);
  expect(text).toContain("PHUX_UNIT_PART: lib");
  expect(text).toContain("PHUX_UNIT_PART: integration");
  expect(needs(job(ci, "ci"))).toEqual([
    "changes",
    "check",
    "test-build",
    "test",
    "test-integration",
    "e2e",
    "uniffi",
    "workflow-gate",
    "integrations",
    "perf-icount",
  ]);

  const recipe = readFileSync(repoPath("just/test.just"), "utf8");
  for (const line of [
    'lib) targets=(--lib --bins); kind="kind(lib) + kind(bin) + kind(proc-macro)" ;;',
    'select(((((."required-features" // []) - $default) == []) | tostring) == $want)',
    "source=(--workspace {{TEST_FEATURES}} ${targets[@]+",
    "targets=(--lib --bins --tests)",
    "cargo nextest archive --workspace {{TEST_FEATURES}}",
    // One feature set: the unit pool, e2e, and stress share a build.
    "source=(--workspace {{TEST_FEATURES}} {{E2E_TARGETS}})",
    "{{TEST_FEATURES}} --features phux-server/stress",
    'TEST_FEATURES := "--features phux-server/measure"',
  ]) {
    expect(recipe).toContain(line);
  }
  expect(recipe).not.toContain("E2E_FEATURES");
});

interface Completed {
  exitCode: number;
  stdout: string;
  stderr: string;
}

function bash(args: string[], env: Record<string, string>): Completed {
  const result = Bun.spawnSync(["bash", ...args], { env, stdout: "pipe", stderr: "pipe" });
  return {
    exitCode: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

describe("a failed main lane", () => {
  test("saves the objects archive", () => {
    // A red assertion must not throw away a finished compile.
    //
    // Run 38457963019 spent 10m 37s compiling and saved nothing because the
    // action exports only from its success post step. The save stays on
    // pushes to main, and only when this restore missed.
    const saveIf =
      "failure() && github.event_name == 'push' && " +
      "github.ref == 'refs/heads/main' && steps.setup.outputs.cache-hit != 'true' " +
      "&& steps.mbx-fail-export.outputs.exported == 'true'";
    const all = Object.values(jobs(workflow("ci.yml"))).flatMap(steps);
    // check, test build, and icount; uniffi's rust-cache saves on failure itself.
    expect(all.filter((step) => step["if"] === saveIf)).toHaveLength(3);
    expect(
      all.filter((step) => run(step).includes("bash scripts/ci/mbx-export-failed-lane.sh")),
    ).toHaveLength(3);
    expect(workflowText("ci.yml")).not.toContain("save-on-pull-request");
    expect(actionText("setup-rust-lane")).toContain(
      "value: ${{ steps.mbx-env.outputs.cache_key }}",
    );
  });

  test("the export script exports only into its own directory", () => {
    const script = repoPath("scripts/ci/mbx-export-failed-lane.sh");
    const scratch = mkdtempSync(join(tmpdir(), "phux-failed-lane-"));
    try {
      const output = join(scratch, "github-output");
      const refused = bash([script, join(scratch, "elsewhere")], { PATH: "/usr/bin:/bin" });
      expect(refused.exitCode).toBe(2);
      const unset = bash([script, join(scratch, "phux-mbx-objects")], {
        PATH: "/usr/bin:/bin",
        GITHUB_OUTPUT: output,
      });
      expect(unset.exitCode).toBe(0);
      expect(existsSync(output) ? readFileSync(output, "utf8") : "").not.toContain("exported=true");

      const bin = join(scratch, "bin");
      mkdirSync(bin);
      // mbx cache export --group GROUP --format directory DEST
      writeFileSync(join(bin, "mbx"), '#!/bin/sh\nmkdir -p "$7"\nexit 0\n', { mode: 0o755 });
      const exported = bash([script, join(scratch, "phux-mbx-objects")], {
        PATH: `${bin}:/usr/bin:/bin`,
        MBX_CACHE_EXPORT_GROUP: "group-1",
        GITHUB_OUTPUT: output,
      });
      expect(exported.exitCode, exported.stderr).toBe(0);
      expect(readFileSync(output, "utf8")).toContain("exported=true");
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  });
});
