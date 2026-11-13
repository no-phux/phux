// Evaluate the workflow's actual, deliberately JS-compatible expressions.
// This is not a general GitHub expression interpreter: these fixtures use
// only boolean operators, format(), and populated contexts.
import { expect, test } from "bun:test";
import { runInNewContext } from "node:vm";
import { capture } from "../lib/process.ts";
import { str, table, type Table } from "../lib/value.ts";
import { Classifier } from "../routing/classify.ts";
import { plan, planOutputs } from "../routing/plan.ts";
import { job, run, steps, workflow } from "./workflows.ts";

function context(event: string, overrides: Table = {}): Table {
  return {
    event_name: event,
    actor: "maintainer",
    repository: "no-phux/phux",
    head_ref: "topic",
    ref_name: "topic",
    ref: "refs/heads/topic",
    sha: "a".repeat(40),
    run_id: 1,
    event: {
      head_commit: { message: "fix: ordinary change" },
      pull_request: {
        number: event === "pull_request" ? 1 : 0,
        head: { repo: { full_name: "no-phux/phux" } },
      },
    },
    ...overrides,
  };
}

function evaluate(value: unknown, github: Table, needs: Table = {}): unknown {
  if (typeof value !== "string") return value;
  const body = value.replace(/^\$\{\{\s*|\s*\}\}$/g, "");
  const result: unknown = runInNewContext(body, {
    github,
    needs,
    cancelled: () => false,
    format: (pattern: string, ...args: string[]) =>
      pattern.replace(/\{(\d+)\}/g, (_, index: string) => String(args[Number(index)])),
  });
  return result;
}

function group(owner: Table, github: Table): string {
  const value = str(table(owner["concurrency"])["group"]) ?? "";
  return value.replace(/\$\{\{(.*?)\}\}/g, (_, body: string) => String(evaluate(body, github)));
}

const CI = workflow("ci.yml");
const FIRST_RUN = workflow("first-run-timing.yml");

test("every main SHA retains its routed work and exact-SHA CI; no push cancels", () => {
  const push = context("push", { ref: "refs/heads/main", ref_name: "main" });
  const newer = { ...push, sha: "b".repeat(40) };
  // A installer/content push followed by a docs-only or Rust-only push
  // must not lose its independent validation/publication admission.
  expect(group(CI, push)).not.toBe(group(CI, newer));
  for (const message of ["chore: release main (#1)", "chore(rune): release 1.2.3 (#2)"]) {
    const release = { ...push, event: { head_commit: { message } } };
    expect(group(CI, release)).toBe(group(CI, push));
    expect(group(CI, release)).not.toBe(group(CI, { ...release, sha: newer.sha }));
  }
  const bot = { ...push, actor: "phux-release-bot[bot]" };
  expect(group(CI, bot)).toBe(group(CI, push));
  expect(evaluate(table(CI["concurrency"])["cancel-in-progress"], push)).toBe(false);
});

test.each<[string, string, string]>([
  ["docs/install.md", "crates/phux-server/src/lib.rs", "content"],
  ["crates/phux-server/src/lib.rs", "docs/install.md", "lint"],
])("pending %s retains %s-independent routed work", (first, second, required) => {
  const classifier = new Classifier();
  const a = context("push", { sha: "a".repeat(40) });
  const b = context("push", { sha: "b".repeat(40) });
  const before = classifier.classify([first]);
  const after = classifier.classify([second]);
  const admitted = new Map([
    [group(CI, a), planOutputs(plan(before, "push"), before)],
    [group(CI, b), planOutputs(plan(after, "push"), after)],
  ]);
  expect(admitted.size).toBe(2);
  expect(admitted.get(group(CI, a))?.[required]).toBe(true);
  expect(admitted.get(group(CI, b))?.[required]).toBe(false);
});

test("PR updates, close and dispatch share a group without crossing repo/ref boundaries", () => {
  const pr = context("pull_request", { ref: "refs/pull/1/merge" });
  const dispatch = context("workflow_dispatch");
  expect(group(CI, pr)).toBe(group(CI, dispatch));
  expect(group(CI, { ...dispatch, ref: "refs/tags/topic" })).not.toBe(group(CI, pr));
  expect(group(CI, { ...pr, ref: "refs/heads/main" })).toBe(group(CI, pr));
  expect(
    group(CI, context("merge_group", { ref: "refs/heads/gh-readonly-queue/main/pr-1" })),
  ).not.toBe(group(CI, dispatch));
  const fork = {
    ...pr,
    head_ref: "bar",
    event: { pull_request: { head: { repo: { full_name: "no-phux/phux-foo" } } } },
  };
  expect(group(CI, fork)).not.toBe(
    group(CI, { ...dispatch, ref_name: "foo-bar", ref: "refs/heads/foo-bar" }),
  );
  for (const event of ["pull_request", "workflow_dispatch", "merge_group"]) {
    expect(evaluate(table(CI["concurrency"])["cancel-in-progress"], context(event))).toBe(true);
  }
});

test("unrelated first-run pushes cannot replace a pending installer or nightly Mac qualification", () => {
  const push = context("push", { ref: "refs/heads/main" });
  // All Linux routing jobs get to classify their event. Coalescing happens
  // only among Mac jobs that the classifier actually selects.
  expect(group(FIRST_RUN, push)).not.toBe(group(FIRST_RUN, { ...push, run_id: 2 }));
  const mac = job(FIRST_RUN, "macos");
  expect(group(mac, push)).toBe(group(mac, { ...push, run_id: 2 }));
  expect(group(mac, push)).not.toBe(group(mac, { ...push, event_name: "schedule" }));
  expect(group(mac, push)).not.toBe(group(mac, { ...push, event_name: "workflow_dispatch" }));
  expect(table(mac["concurrency"])["cancel-in-progress"]).toBe(false);
});

test("first-run Mac routing keeps installer PR/push coverage and nightly/manual qualification", () => {
  const mac = job(FIRST_RUN, "macos");
  for (const event of ["pull_request", "push", "schedule", "workflow_dispatch"]) {
    expect(evaluate(table(FIRST_RUN["concurrency"])["cancel-in-progress"], context(event))).toBe(
      event === "pull_request",
    );
    for (const installer of ["true", "false"]) {
      const selected = evaluate(mac["if"], context(event), { linux: { outputs: { installer } } });
      expect([event, installer, selected]).toEqual([
        event,
        installer,
        installer === "true" || ["schedule", "workflow_dispatch"].includes(event),
      ]);
    }
  }
  expect(table(FIRST_RUN["on"])["schedule"]).toEqual([{ cron: "29 4 * * *" }]);
});

test("first-run push classifier fails closed for unknown history or failed diffs", () => {
  const changes = steps(job(FIRST_RUN, "linux")).find((step) => step["id"] === "push_changes");
  const script = run(changes ?? {});
  expect(script).not.toBe("");
  for (const path of [
    "scripts/install.sh",
    "scripts/installer",
    "scripts/generate-install.sh",
    "scripts/first-run-timing.sh",
    ".github/workflows/first-run-timing.yml",
  ]) {
    expect(script).toContain(path);
    const pr = steps(job(FIRST_RUN, "linux")).find((step) => step["id"] === "changes");
    expect(run(pr ?? {})).toContain(path);
  }
  for (const [before, failedFetch, failedDiff, changed, installer] of [
    ["", "0", "0", "", "true"],
    ["0".repeat(40), "0", "0", "", "true"],
    ["a".repeat(40), "1", "0", "", "true"],
    ["a".repeat(40), "0", "1", "", "true"],
    ["a".repeat(40), "0", "0", "scripts/install.sh", "true"],
    ["a".repeat(40), "0", "0", "", "false"],
  ]) {
    const output = capture(
      [
        "bash",
        "-e",
        "-c",
        `
        git() {
          if [ "$1" = fetch ]; then return "$FAILED_FETCH"; fi
          if [ "$FAILED_DIFF" != 0 ]; then return "$FAILED_DIFF"; fi
          printf '%s' "$CHANGED"
      }
      ${script}
    `,
      ],
      {
        env: {
          ...process.env,
          BEFORE_SHA: before,
          AFTER_SHA: "b".repeat(40),
          FAILED_FETCH: failedFetch,
          FAILED_DIFF: failedDiff,
          CHANGED: changed,
          GITHUB_OUTPUT: "/dev/stdout",
        },
      },
    );
    expect(output).toContain(`installer=${installer}`);
  }
});
