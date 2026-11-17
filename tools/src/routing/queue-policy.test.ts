import { expect, test } from "bun:test";
import { Classifier } from "./classify.ts";
import { plan, planOutputs } from "./plan.ts";
import { job, run, steps, workflow } from "../policy/workflows.ts";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { capture } from "../lib/process.ts";
import { repoPath } from "../lib/repo.ts";
import { mergeQueueVerified } from "./queue-policy.ts";
import { classifyDetection } from "./detect.ts";
import { runInNewContext } from "node:vm";

// GitHub repository-rule-merge-queue parameters, not a type-only queue claim.
const QUEUE_PARAMETERS = {
  check_response_timeout_minutes: 60,
  grouping_strategy: "ALLGREEN",
  max_entries_to_build: 5,
  max_entries_to_merge: 5,
  merge_method: "REBASE",
  min_entries_to_merge: 1,
  min_entries_to_merge_wait_minutes: 5,
};
const queueRules = (parameters: unknown) => JSON.stringify([{ type: "merge_queue", parameters }]);
const MISSING_PARAMETERS = Object.keys(QUEUE_PARAMETERS).map((field) => {
  const parameters: Record<string, unknown> = { ...QUEUE_PARAMETERS };
  delete parameters[field];
  return [`queue missing ${field}`, queueRules(parameters), 0, false] as const;
});
const RESPONSES = [
  ["queue on", queueRules(QUEUE_PARAMETERS), 0, true],
  [
    "queue at schema lower bounds",
    queueRules({
      check_response_timeout_minutes: 1,
      grouping_strategy: "HEADGREEN",
      max_entries_to_build: 0,
      max_entries_to_merge: 0,
      merge_method: "MERGE",
      min_entries_to_merge: 0,
      min_entries_to_merge_wait_minutes: 0,
    }),
    0,
    true,
  ],
  [
    "queue at schema upper bounds",
    queueRules({
      check_response_timeout_minutes: 360,
      grouping_strategy: "HEADGREEN",
      max_entries_to_build: 100,
      max_entries_to_merge: 100,
      merge_method: "SQUASH",
      min_entries_to_merge: 100,
      min_entries_to_merge_wait_minutes: 360,
    }),
    0,
    true,
  ],
  ...MISSING_PARAMETERS,
  ["queue with null parameters", queueRules(null), 0, false],
  ["queue with missing parameters", '[{"type":"merge_queue"}]', 0, false],
  ["queue with incomplete parameters", queueRules({ grouping_strategy: "ALLGREEN" }), 0, false],
  [
    "queue with negative build limit",
    queueRules({ ...QUEUE_PARAMETERS, max_entries_to_build: -1 }),
    0,
    false,
  ],
  [
    "queue with invalid grouping",
    queueRules({ ...QUEUE_PARAMETERS, grouping_strategy: "GREEN" }),
    0,
    false,
  ],
  [
    "queue with invalid merge method",
    queueRules({ ...QUEUE_PARAMETERS, merge_method: "rebase" }),
    0,
    false,
  ],
  [
    "queue with wrong timeout type",
    queueRules({ ...QUEUE_PARAMETERS, check_response_timeout_minutes: "60" }),
    0,
    false,
  ],
  [
    "queue with zero timeout",
    queueRules({ ...QUEUE_PARAMETERS, check_response_timeout_minutes: 0 }),
    0,
    false,
  ],
  [
    "queue with fractional wait",
    queueRules({ ...QUEUE_PARAMETERS, min_entries_to_merge_wait_minutes: 1.5 }),
    0,
    false,
  ],
  [
    "queue with inverted merge limits",
    queueRules({ ...QUEUE_PARAMETERS, min_entries_to_merge: 6 }),
    0,
    false,
  ],
  [
    "queue with excessive build limit",
    queueRules({ ...QUEUE_PARAMETERS, max_entries_to_build: 101 }),
    0,
    false,
  ],
  [
    "valid queue beside malformed queue",
    JSON.stringify([
      { type: "merge_queue", parameters: QUEUE_PARAMETERS },
      { type: "merge_queue", parameters: null },
    ]),
    0,
    false,
  ],
  ["queue off", '[{"type":"pull_request"}]', 0, false],
  ["empty", "[]", 0, false],
  ["malformed JSON", "{", 0, false],
  ["wrong envelope", '{"rules":[{"type":"merge_queue"}]}', 0, false],
  ["malformed rule", '[null,{"type":"merge_queue"}]', 0, false],
  ["missing type", '[{},{"type":"merge_queue"}]', 0, false],
  ["403", '{"message":"Resource not accessible by integration"}', 1, false],
  ["API error", "", 1, false],
] as const;

function fixture() {
  const dir = mkdtempSync(join(tmpdir(), "queue-policy-"));
  const sha = capture(["git", "rev-parse", "HEAD"]).trim();
  const event = {
    pull_request: {
      base: { sha, ref: "main", repo: { full_name: "no-phux/phux" } },
      head: { sha, ref: "gh-readonly-queue/main/fake", repo: { full_name: "fork/phux" } },
    },
  };
  const eventPath = join(dir, "event.json");
  writeFileSync(eventPath, JSON.stringify(event));
  return { dir, event, eventPath };
}

test.each(RESPONSES)(
  "%s reads the base branch rules once and fails closed",
  (_, body, code, want) => {
    const { dir, event, eventPath } = fixture();
    const paths: string[] = [];
    try {
      expect(
        mergeQueueVerified("no-phux/phux", eventPath, {
          json(path) {
            paths.push(path);
            if (code !== 0) throw new Error(body);
            return JSON.parse(body) as unknown;
          },
        }),
      ).toBe(want);
      expect(paths).toEqual(["repos/no-phux/phux/rules/branches/main"]);
      event.pull_request.base.ref = "release/a b?#";
      writeFileSync(eventPath, JSON.stringify(event));
      mergeQueueVerified("no-phux/phux", eventPath, {
        json(path) {
          expect(path).toBe("repos/no-phux/phux/rules/branches/release%2Fa%20b%3F%23");
          return [];
        },
      });
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  },
);

test("missing or foreign base context cannot claim the source fork's queue policy", () => {
  const { dir, event, eventPath } = fixture();
  let queries = 0;
  const source = {
    json: () => {
      queries++;
      return [{ type: "merge_queue" }];
    },
  };
  try {
    expect(mergeQueueVerified("no-phux/phux", undefined, source)).toBe(false);
    event.pull_request.base.repo.full_name = "fork/phux";
    writeFileSync(eventPath, JSON.stringify(event));
    expect(mergeQueueVerified("no-phux/phux", eventPath, source)).toBe(false);
    writeFileSync(eventPath, "{}");
    expect(mergeQueueVerified("no-phux/phux", eventPath, source)).toBe(false);
    expect(queries).toBe(0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test.each(RESPONSES)(
  "%s controls the public ci-plan and required aggregate",
  (_, body, code, verified) => {
    const { dir, eventPath } = fixture();
    try {
      writeFileSync(
        join(dir, "gh"),
        `#!/bin/sh\nprintf '%s\\n' "$*" >> "$POLICY_LOG"\nprintf '%s' "$POLICY_RESPONSE"\nexit "$POLICY_CODE"\n`,
        { mode: 0o755 },
      );
      const realGit = capture(["which", "git"]).trim();
      writeFileSync(
        join(dir, "git"),
        `#!/bin/sh\nif [ "$1" = diff ]; then printf 'clients/rune/src/main.rs\\0'; else exec "${realGit}" "$@"; fi\n`,
        { mode: 0o755 },
      );
      const outputs = join(dir, "outputs");
      const log = join(dir, "queries");
      capture([process.execPath, repoPath("tools/main.ts"), "ci-plan"], {
        env: {
          ...process.env,
          PATH: `${dir}:${process.env["PATH"] ?? ""}`,
          GITHUB_EVENT_NAME: "pull_request",
          GITHUB_EVENT_PATH: eventPath,
          GITHUB_REPOSITORY: "no-phux/phux",
          GITHUB_OUTPUT: outputs,
          POLICY_LOG: log,
          POLICY_RESPONSE: body,
          POLICY_CODE: String(code),
        },
      });
      const text = readFileSync(outputs, "utf8");
      const output = Object.fromEntries(
        text
          .trim()
          .split("\n")
          .map((line) => {
            const index = line.indexOf("=");
            return [line.slice(0, index), line.slice(index + 1)];
          }),
      );
      expect(readFileSync(log, "utf8").trim().split("\n")).toEqual([
        "api repos/no-phux/phux/rules/branches/main",
      ]);
      expect(output).toMatchObject({
        queue_verified: String(verified),
        rune: "true",
        rune_qualify: String(!verified),
        native: String(!verified),
        icount: String(!verified),
      });
      const appleGate = job(workflow("ci.yml"), "queue-apple")["if"];
      expect(
        runInNewContext(String(appleGate), {
          github: { event_name: "pull_request" },
          needs: { plan: { outputs: output } },
        }),
      ).toBe(!verified);
      const applePlan = capture([process.execPath, repoPath("tools/main.ts"), "rune-macos-plan"], {
        env: {
          ...process.env,
          GITHUB_EVENT_NAME: "pull_request",
          GITHUB_EVENT_PATH: eventPath,
          GITHUB_OUTPUT: "",
          PHUX_RUNE_QUALIFY: String(!verified),
        },
      });
      expect(applePlan).toContain(`macos=${!verified}`);
      expect(applePlan).toContain(`ios=${!verified}`);
      const receipt = JSON.parse(output["receipt"] ?? "{}") as { coverage: string };
      expect(receipt.coverage.includes("packaging")).toBe(!verified);
      const script = run(
        steps(job(workflow("ci.yml"), "ci")).find((step) => run(step) !== "") ?? {},
      );
      const baseline = {
        plan: { result: "success", outputs: output },
        native: { result: verified ? "skipped" : "success" },
        icount: { result: verified ? "skipped" : "success" },
        "queue-apple": { result: verified ? "skipped" : "success" },
        rune: { result: "success" },
      };
      for (const lane of ["native", "icount", "queue-apple", "rune"]) {
        for (const state of ["success", "skipped", "failure", "cancelled"]) {
          const result = Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
            cwd: repoPath("."),
            env: {
              ...process.env,
              EVENT_NAME: "pull_request",
              RESULTS: JSON.stringify({ ...baseline, [lane]: { result: state } }),
            },
            stdout: "ignore",
            stderr: "ignore",
          }).exitCode;
          const allowed = state === "success" || (verified && state === "skipped");
          expect([lane, state, result]).toEqual([lane, state, allowed ? 0 : 1]);
        }
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  },
);

test.each([
  "Cargo.toml",
  "clients/rune/Cargo.lock",
  ".cargo/config.toml",
  "crates/phux-protocol/Cargo.toml",
])("%s retains full Linux and mandatory qualification without a queue", (path) => {
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

test("unknown queue enforcement cannot waive PR qualification", () => {
  const paths = ["clients/rune/src/main.rs"];
  const c = new Classifier().classify(paths);
  const p = plan(c, "pull_request", { scope: { kind: "diff", paths } });
  expect(planOutputs(p, c)).toMatchObject({ native: true, icount: true, rune_qualify: true });
  expect(p.coverage.split(",")).toContain("packaging");
  const explicitCheap = plan(c, "pull_request", {
    scope: { kind: "diff", paths },
    queueVerified: false,
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

test.each(["native", "icount", "queue-apple", "rune"])(
  "the required PR aggregate rejects a skipped %s without verified queue enforcement",
  (lane) => {
    const script = run(steps(job(workflow("ci.yml"), "ci")).find((step) => run(step) !== "") ?? {});
    const results = {
      plan: { result: "success", outputs: { queue_verified: "false", rune: "true" } },
      native: { result: "success" },
      icount: { result: "success" },
      "queue-apple": { result: "success" },
      rune: { result: "success" },
      [lane]: { result: "skipped" },
    };
    expect(
      Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
        cwd: repoPath("."),
        env: { ...process.env, EVENT_NAME: "pull_request", RESULTS: JSON.stringify(results) },
        stdout: "ignore",
      }).exitCode,
    ).toBe(1);
  },
);
