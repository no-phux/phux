// Pull requests never wait on the macOS pool: they gate on their path-scoped
// Linux lanes, and main pushes qualify Apple, Linux packaging, native setup
// and icount (docs/ci.md#native-apple-qualification).
import { expect, test } from "bun:test";
import { Classifier } from "./classify.ts";
import { LANES, plan, planOutputs } from "./plan.ts";
import { job, needs, run, steps, workflow } from "../policy/workflows.ts";
import { table } from "../lib/value.ts";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { capture } from "../lib/process.ts";
import { repoPath } from "../lib/repo.ts";
import { classifyDetection } from "./detect.ts";
import { runInNewContext } from "node:vm";

function fixture(ref = "topic", headRepo = "no-phux/phux") {
  const dir = mkdtempSync(join(tmpdir(), "pr-policy-"));
  const sha = capture(["git", "rev-parse", "HEAD"]).trim();
  const event = {
    pull_request: {
      base: { sha, ref: "main", repo: { full_name: "no-phux/phux" } },
      head: { sha, ref, repo: { full_name: headRepo } },
    },
  };
  const eventPath = join(dir, "event.json");
  writeFileSync(eventPath, JSON.stringify(event));
  return { dir, event, eventPath };
}

function aggregate(event: string, results: Record<string, unknown>): number | null {
  const script = run(steps(job(workflow("ci.yml"), "ci")).find((step) => run(step) !== "") ?? {});
  const defaults = Object.fromEntries(
    [...LANES, "rune_qualify", "release_qualify"].map((key) => [key, "false"]),
  );
  const complete = {
    ...Object.fromEntries(
      needs(job(workflow("ci.yml"), "ci")).map((id) => [id, { result: "skipped" }]),
    ),
    guards: { result: "success" },
    ...results,
    plan: {
      ...table(results["plan"]),
      outputs: { ...defaults, ...table(table(results["plan"])["outputs"]) },
    },
  };
  return Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
    cwd: repoPath("."),
    env: { ...process.env, EVENT_NAME: event, RESULTS: JSON.stringify(complete) },
    stdout: "ignore",
    stderr: "ignore",
  }).exitCode;
}

/** The public ci-plan outputs for a Rune-only pull request from `ref`. */
function planRunePullRequest(ref: string, headRepo?: string): Record<string, string> {
  const { dir, eventPath } = fixture(ref, headRepo);
  try {
    // No gh on PATH: the PR plan must not read repository rules.
    writeFileSync(join(dir, "gh"), "#!/bin/sh\nexit 97\n", { mode: 0o755 });
    const realGit = capture(["which", "git"]).trim();
    writeFileSync(
      join(dir, "git"),
      `#!/bin/sh\nif [ "$1" = diff ]; then printf 'clients/rune/src/main.rs\\0'; else exec "${realGit}" "$@"; fi\n`,
      { mode: 0o755 },
    );
    const outputs = join(dir, "outputs");
    capture([process.execPath, repoPath("tools/main.ts"), "ci-plan"], {
      env: {
        ...process.env,
        PATH: `${dir}:${process.env["PATH"] ?? ""}`,
        GITHUB_EVENT_NAME: "pull_request",
        GITHUB_EVENT_PATH: eventPath,
        GITHUB_REPOSITORY: "no-phux/phux",
        GITHUB_OUTPUT: outputs,
      },
    });
    return Object.fromEntries(
      readFileSync(outputs, "utf8")
        .trim()
        .split("\n")
        .map((line) => {
          const index = line.indexOf("=");
          return [line.slice(0, index), line.slice(index + 1)];
        }),
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function appleAdmitted(output: Record<string, string>): boolean {
  const appleGate = job(workflow("ci.yml"), "queue-apple")["if"];
  return runInNewContext(String(appleGate), {
    github: { event_name: "pull_request" },
    needs: { plan: { outputs: output } },
  }) as boolean;
}

test.each([
  ["topic", "no-phux/phux"],
  ["release-please--branches--main", "fork/phux"],
])(
  "a Rune pull request from %s (%s) plans Linux only and its aggregate never waits on Apple",
  (ref, headRepo) => {
    const output = planRunePullRequest(ref, headRepo);
    expect(output["queue_verified"]).toBeUndefined();
    expect(output).toMatchObject({
      release_qualify: "false",
      rune: "true",
      rune_qualify: "false",
      native: "false",
      icount: "false",
    });
    const receipt = JSON.parse(output["receipt"] ?? "{}") as { coverage: string };
    expect(receipt.coverage.includes("packaging")).toBe(false);
    expect(appleAdmitted(output)).toBe(false);
    const baseline = {
      plan: { result: "success", outputs: output },
      native: { result: "skipped" },
      icount: { result: "skipped" },
      "queue-apple": { result: "skipped" },
      rune: { result: "success" },
    };
    for (const lane of ["native", "icount", "queue-apple", "rune"]) {
      for (const state of ["success", "skipped", "failure", "cancelled"]) {
        const result = aggregate("pull_request", { ...baseline, [lane]: { result: state } });
        const allowed = state === "success" || (state === "skipped" && lane !== "rune");
        expect([lane, state, result]).toEqual([lane, state, allowed ? 0 : 1]);
      }
    }
  },
  15_000,
);

test.each(["release-please--branches--main", "rune-release/next"])(
  "a release bot's PR from %s keeps full qualification, Apple included",
  (ref) => {
    const output = planRunePullRequest(ref);
    expect(output).toMatchObject({
      release_qualify: "true",
      rune: "true",
      rune_qualify: "true",
      native: "true",
      icount: "true",
    });
    const receipt = JSON.parse(output["receipt"] ?? "{}") as { coverage: string };
    expect(receipt.coverage.includes("packaging")).toBe(true);
    expect(appleAdmitted(output)).toBe(true);
    const baseline = {
      plan: { result: "success", outputs: output },
      native: { result: "success" },
      icount: { result: "success" },
      "queue-apple": { result: "success" },
      rune: { result: "success" },
    };
    expect(aggregate("pull_request", baseline)).toBe(0);
    for (const lane of ["native", "icount", "queue-apple", "rune"]) {
      expect([
        lane,
        aggregate("pull_request", { ...baseline, [lane]: { result: "skipped" } }),
      ]).toEqual([lane, 1]);
    }
  },
  15_000,
);

test.each(["native", "icount", "queue-apple"])(
  "the merge-group aggregate rejects a skipped %s",
  (lane) => {
    const results = {
      plan: { result: "success", outputs: { rune: "true" } },
      native: { result: "success" },
      icount: { result: "success" },
      "queue-apple": { result: "success" },
      rune: { result: "success" },
    };
    expect(aggregate("merge_group", results)).toBe(0);
    expect(aggregate("merge_group", { ...results, [lane]: { result: "skipped" } })).toBe(1);
  },
);

test.each(["native", "icount"])("the main push aggregate rejects a skipped %s", (lane) => {
  const results = {
    plan: { result: "success", outputs: {} },
    native: { result: "success" },
    icount: { result: "success" },
    "queue-apple": { result: "skipped" },
  };
  expect(aggregate("push", results)).toBe(0);
  expect(aggregate("push", { ...results, [lane]: { result: "skipped" } })).toBe(1);
});

test.each([
  "Cargo.toml",
  "clients/rune/Cargo.lock",
  ".cargo/config.toml",
  "crates/phux-protocol/Cargo.toml",
])("%s plans full qualification when a caller does not ask for the fast path", (path) => {
  const c = new Classifier().classify([path]);
  const output = planOutputs(
    plan(c, "pull_request", { scope: { kind: "diff", paths: [path] } }),
    c,
  );
  expect(output).toMatchObject({ native: true, icount: true, rune: true, rune_qualify: true });
});

test("release metadata fallback qualifies Rune rather than dropping exact-tree native proof", () => {
  const c = classifyDetection({ kind: "release-metadata" }, "qualify-rune");
  expect(planOutputs(plan(c, "pull_request", { scope: { kind: "none" } }), c)).toMatchObject({
    native: true,
    icount: true,
    rune: true,
    rune_qualify: true,
  });
});

test("an omitted or false fast path cannot waive PR qualification", () => {
  const paths = ["clients/rune/src/main.rs"];
  const c = new Classifier().classify(paths);
  const p = plan(c, "pull_request", { scope: { kind: "diff", paths } });
  expect(planOutputs(p, c)).toMatchObject({ native: true, icount: true, rune_qualify: true });
  expect(p.coverage.split(",")).toContain("packaging");
  const explicitCheap = plan(c, "pull_request", {
    scope: { kind: "diff", paths },
    pullRequestFast: false,
    runeQualify: false,
    mergeQueue: true,
  });
  expect(planOutputs(explicitCheap, c)).toMatchObject({
    native: true,
    icount: true,
    rune_qualify: true,
  });
});

test.each(["true", "false"])(
  "the public Apple child plan qualifies a PR only with caller input %s",
  (qualify) => {
    const { dir, eventPath } = fixture();
    try {
      const text = capture([process.execPath, repoPath("tools/main.ts"), "rune-macos-plan"], {
        env: {
          ...process.env,
          GITHUB_EVENT_NAME: "pull_request",
          GITHUB_EVENT_PATH: eventPath,
          GITHUB_OUTPUT: "",
          PHUX_RUNE_QUALIFY: qualify,
        },
      });
      expect(text).toContain(`macos=${qualify}`);
      expect(text).toContain(`ios=${qualify}`);
      const receiptLine = text.split("\n").find((line) => line.startsWith("receipt=")) ?? "";
      expect(JSON.parse(receiptLine.slice("receipt=".length)) as unknown).toMatchObject({
        coverage: qualify === "true" ? "macos,rune_ios" : "",
      });
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  },
);
