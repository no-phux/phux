// Execute the press's real shell with local gh/bun fixtures. No GitHub writes.
import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { strings } from "../lib/value.ts";
import { job, run, steps, uses, withInput, workflow, workflowText } from "../policy/workflows.ts";

const HEAD = "3bbef2bd820c1c16118c96d0d2e6fa2c216149c6";
const OTHER = "e2f9f3d69e3dd5a45105761c55b23de160dfbef8";
const READY = { number: 42, isDraft: false, headRefOid: HEAD };
const CURRENT = { state: "OPEN", isDraft: false, headRefOid: HEAD };
const PRESS = steps(job(workflow("phux-auto-release.yml"), "press")).find((step) =>
  run(step).includes("pr-ready-ci"),
);
if (PRESS === undefined) throw new Error("missing release press");
const SCRIPT = run(PRESS);

interface Fixture {
  pr?: typeof READY | null;
  current?: typeof CURRENT;
  guard?: number;
  armed?: boolean;
  last?: string;
  releaseEpoch?: number;
  listError?: boolean;
  viewError?: boolean;
}

const MOCK = `#!${process.execPath}
import { appendFileSync } from "node:fs";
import { basename } from "node:path";
const command = basename(process.argv[1]);
const args = process.argv.slice(2);
const fixture = JSON.parse(process.env.FIXTURE);
appendFileSync(process.env.TRACE, JSON.stringify([command, ...args]) + "\\n");
const output = (value) => console.log(value);
const gh = {
  "pr list": () => {
    if (fixture.listError) process.exit(1);
    if (fixture.armed !== undefined) return output(String(fixture.armed));
    output(fixture.pr === null ? "" : JSON.stringify(fixture.pr));
  },
  "pr view": () => {
    if (fixture.viewError) process.exit(1);
    output(JSON.stringify(fixture.current));
  },
  "pr ready": () => {},
  "pr merge": () => {},
  "release list": () => output(fixture.last ?? ""),
};
const commands = {
  gh: () => {
    const handler = gh[args.slice(0, 2).join(" ")];
    if (!handler) throw new Error("unexpected gh command " + args);
    handler();
  },
  bun: () => process.exit(fixture.guard ?? 0),
  date: () => output(args[0] === "+%s" ? 20000 : fixture.releaseEpoch),
};
commands[command]();
`;

function execute(
  fixture: Fixture,
  environment: Record<string, string> = {},
  script = SCRIPT,
): { calls: string[][]; stdout: string; output: string; code: number } {
  const dir = mkdtempSync(join(tmpdir(), "release-press-"));
  try {
    for (const name of ["gh", "bun", "date"]) {
      writeFileSync(join(dir, name), MOCK, { mode: 0o755 });
    }
    const trace = join(dir, "trace");
    const output = join(dir, "output");
    writeFileSync(trace, "");
    writeFileSync(output, "");
    const result = Bun.spawnSync(["bash", "-c", script], {
      env: {
        ...process.env,
        PATH: `${dir}:${process.env.PATH}`,
        GH_TOKEN: "fixture",
        GH_REPO: "no-phux/phux",
        EVENT: "workflow_dispatch",
        RUN_EVENT: "",
        RUN_CONCLUSION: "",
        RUN_BRANCH: "",
        CADENCE_SECONDS: "7200",
        GITHUB_OUTPUT: output,
        FIXTURE: JSON.stringify({ pr: READY, current: CURRENT, ...fixture }),
        TRACE: trace,
        ...environment,
      },
    });
    if (result.stderr.length > 0) throw new Error(result.stderr.toString());
    return {
      calls: readFileSync(trace, "utf8")
        .split("\n")
        .filter(Boolean)
        .map((line) => {
          const value: unknown = JSON.parse(line);
          return strings(value);
        }),
      stdout: result.stdout.toString(),
      output: readFileSync(output, "utf8"),
      code: result.exitCode,
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function mutations(calls: string[][]): string[][] {
  return calls.filter(
    ([command, resource, verb]) =>
      command === "gh" && resource === "pr" && (verb === "ready" || verb === "merge"),
  );
}

test("a draft is armed once, never granted auto-merge in the arming run", () => {
  const result = execute({ pr: { ...READY, isDraft: true } });
  expect(result.code).toBe(0);
  expect(mutations(result.calls)).toEqual([["gh", "pr", "ready", "42"]]);
  expect(result.calls.some(([command]) => command === "bun")).toBe(false);
  expect(result.stdout).toContain("waiting for genuine post-ready ci");
});

test("guard pending, failed and unreadable outcomes leave the armed head alone", () => {
  for (const guard of [1, 2, 3]) {
    const result = execute({ guard });
    expect(result.code).toBe(0);
    expect(mutations(result.calls)).toEqual([]);
    expect(result.calls.at(-1)).toEqual(["bun", "tools/main.ts", "pr-ready-ci", "42", HEAD]);
    expect(result.stdout).toContain("guard did not authorize auto-merge");
  }
});

test("success is requeried before auto-merge and both commands bind the full head", () => {
  const result = execute({});
  expect(result.code).toBe(0);
  expect(result.calls.slice(1)).toEqual([
    ["bun", "tools/main.ts", "pr-ready-ci", "42", HEAD],
    ["gh", "pr", "view", "42", "--json", "state,isDraft,headRefOid"],
    ["gh", "pr", "merge", "42", "--auto", "--squash", "--match-head-commit", HEAD],
  ]);
  expect(mutations(result.calls)).toHaveLength(1);
});

test("a newer head, current draft, or closed PR invalidates even a successful guard", () => {
  for (const current of [
    { ...CURRENT, headRefOid: OTHER },
    { ...CURRENT, isDraft: true },
    { ...CURRENT, state: "CLOSED" },
    { ...CURRENT, state: "MERGED" },
  ]) {
    const result = execute({ current });
    expect(result.code).toBe(0);
    expect(mutations(result.calls)).toEqual([]);
    expect(result.stdout).toContain("changed after the guard");
  }
});

test("no pending PR is a no-op, but unreadable state fails closed", () => {
  const absent = execute({ pr: null });
  expect(absent.code).toBe(0);
  expect(mutations(absent.calls)).toEqual([]);
  for (const fixture of [{ listError: true }, { viewError: true }]) {
    const result = execute(fixture);
    expect(result.code).not.toBe(0);
    expect(mutations(result.calls)).toEqual([]);
  }
});

test("a draft's own ci completion never arms it", () => {
  const result = execute(
    { pr: { ...READY, isDraft: true } },
    {
      EVENT: "workflow_run",
      RUN_EVENT: "pull_request",
      RUN_CONCLUSION: "success",
      RUN_BRANCH: "release-please--branches--main",
    },
  );
  expect(result.code).toBe(0);
  expect(mutations(result.calls)).toEqual([]);
});

test("only a green main push may arm on a completion wake-up", () => {
  const event = {
    EVENT: "workflow_run",
    RUN_EVENT: "push",
    RUN_CONCLUSION: "success",
    RUN_BRANCH: "main",
  };
  const draft = { pr: { ...READY, isDraft: true } };
  expect(mutations(execute(draft, event).calls)).toEqual([["gh", "pr", "ready", "42"]]);
  for (const change of [
    { RUN_EVENT: "workflow_dispatch" },
    { RUN_CONCLUSION: "failure" },
    { RUN_BRANCH: "topic" },
  ]) {
    expect(mutations(execute(draft, { ...event, ...change }).calls)).toEqual([]);
  }
});

test("the schedule respects cadence; dispatch bypasses it, not the CI guard", () => {
  const fixture = {
    pr: { ...READY, isDraft: true },
    last: "2026-11-22T00:00:00Z",
    releaseEpoch: 19000,
  };
  const waiting = execute(fixture, { EVENT: "schedule" });
  expect(waiting.code).toBe(0);
  expect(mutations(waiting.calls)).toEqual([]);
  expect(waiting.stdout).toContain("last stable release was 1000s ago");
  expect(
    mutations(execute({ ...fixture, releaseEpoch: 10000 }, { EVENT: "schedule" }).calls),
  ).toEqual([["gh", "pr", "ready", "42"]]);
  const dispatch = execute(fixture);
  expect(mutations(dispatch.calls)).toEqual([["gh", "pr", "ready", "42"]]);
  expect(dispatch.calls.some((args) => args[1] === "release")).toBe(false);
  expect(mutations(execute({ guard: 1 }).calls)).toEqual([]);
});

test("Release Please freezes PR rewriting only, keeps tag creation and fails closed", () => {
  const release = job(workflow("release.yml"), "release-please");
  const armed = steps(release).find((step) => step["id"] === "armed");
  if (armed === undefined) throw new Error("missing armed check");
  const action = steps(release).find((step) => uses(step).startsWith("googleapis/release-please"));
  if (action === undefined) throw new Error("missing Release Please action");
  expect(steps(release).indexOf(armed)).toBeLessThan(steps(release).indexOf(action));
  expect(action["if"]).toBeUndefined();
  expect(withInput(action, "skip-github-pull-request")).toBe(
    "${{ steps.armed.outputs.armed == 'true' }}",
  );
  expect(withInput(action, "skip-github-release")).toBeUndefined();
  for (const value of [true, false]) {
    const result = execute({ armed: value }, {}, run(armed));
    expect(result.code).toBe(0);
    expect(result.output).toBe(`armed=${value}\n`);
  }
  expect(execute({ listError: true }, {}, run(armed)).code).not.toBe(0);
  expect(run(armed)).toContain("(.isCrossRepository | not) and (.isDraft | not)");
});

test("both trains use pinned Bun from a main checkout, without blind reruns or draft resets", () => {
  for (const [name, id] of [
    ["phux-auto-release.yml", "press"],
    ["rune-auto-release.yml", "step"],
  ] as const) {
    const all = steps(job(workflow(name), id));
    const checkout = all.find((step) => uses(step).startsWith("actions/checkout@"));
    expect(checkout).toBeDefined();
    expect(withInput(checkout ?? {}, "ref")).toBe("main");
    const setup = all.findIndex((step) => uses(step) === "./.github/actions/setup-bun");
    expect(setup).toBeGreaterThanOrEqual(0);
    const train = all.findIndex((step) =>
      /pr-ready-ci|python3 clients\/rune\/scripts\/release.py/.test(run(step)),
    );
    expect(train).toBeGreaterThan(setup);
    expect(workflowText(name)).not.toMatch(/gh run rerun|pr ready .*--undo|actions: write/);
  }
  expect(SCRIPT).toContain("select(.isCrossRepository | not)");
});
