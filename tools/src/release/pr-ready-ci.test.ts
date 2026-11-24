import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { Api } from "../lib/github.ts";
import { ROOT } from "../lib/repo.ts";
import { table, type Table } from "../lib/value.ts";
import { prReadyCi } from "./pr-ready-ci.ts";
import { STRICT_AGGREGATE_STEP } from "./pr-ready-proof.ts";

const REPO = "no-phux/phux";
const SHA = "a".repeat(40);
const PREFIX = `repos/${REPO}`;
const PULL = `${PREFIX}/pulls/7`;
const TIMELINE = `${PREFIX}/issues/7/timeline?per_page=100&page=1`;
const STATUSES = `${PREFIX}/commits/${SHA}/status?per_page=100&page=1`;
const READY = "2026-11-23T01:00:00Z";
const LATER = "2026-11-23T01:00:02Z";
const scratch = mkdtempSync(join(tmpdir(), "pr-ready-ci-"));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

function query(workflow: string): string {
  return `${PREFIX}/actions/workflows/${workflow}/runs?head_sha=${SHA}&event=pull_request&per_page=100&page=1`;
}

function job(name: string): Table {
  return { name, status: "completed", conclusion: "success" };
}

function run(id: number, workflow: string): Table {
  return {
    id,
    run_attempt: 1,
    path: `.github/workflows/${workflow}`,
    repository: { full_name: REPO },
    head_repository: { full_name: REPO },
    head_sha: SHA,
    event: "pull_request",
    pull_requests: [{ number: 7 }],
    status: "completed",
    conclusion: "success",
    created_at: LATER,
  };
}

function fixture() {
  const data: Record<string, unknown> = {
    [PULL]: {
      number: 7,
      state: "open",
      draft: false,
      merged: false,
      head: { sha: SHA, repo: { full_name: REPO } },
      base: { ref: "main", repo: { full_name: REPO } },
      created_at: "2026-11-22T00:00:00Z",
      updated_at: READY,
    },
    [TIMELINE]: [{ event: "ready_for_review", created_at: READY }],
    [query("ci.yml")]: { workflow_runs: [run(20, "ci.yml")] },
    [query("commitlint.yml")]: { workflow_runs: [run(21, "commitlint.yml")] },
    [`${PREFIX}/actions/runs/20/attempts/1/jobs?per_page=100&page=1`]: {
      jobs: [job("plan"), { ...job("ci"), steps: [job(STRICT_AGGREGATE_STEP)] }],
    },
    [`${PREFIX}/actions/runs/21/attempts/1/jobs?per_page=100&page=1`]: {
      jobs: [job("commitlint")],
    },
    [STATUSES]: { statuses: [] },
  };
  const calls: string[] = [];
  const client: Api = {
    json(path) {
      calls.push(path);
      if (!(path in data)) throw new Error(`unexpected API request: ${path}`);
      return structuredClone(data[path]);
    },
    bytes() {
      throw new Error("no artifacts are needed");
    },
  };
  return { data, calls, client };
}

test("strict normal post-Ready evidence passes; no legacy status is required", () => {
  const { client, calls } = fixture();
  expect(prReadyCi(REPO, 7, SHA, client)).toBe(0);
  expect(calls.filter((path) => path === PULL)).toHaveLength(2);
  expect(calls.filter((path) => path === TIMELINE)).toHaveLength(2);
});

test.each([
  { draft: true },
  { state: "closed" },
  { merged: true },
  { head: { sha: "b".repeat(40), repo: { full_name: REPO } } },
  { head: { sha: SHA, repo: { full_name: "fork/phux" } } },
  { base: { repo: { full_name: "fork/phux" } } },
  { base: { ref: "release", repo: { full_name: REPO } } },
])("draft, closed, fork or changed-head PR refuses before reading CI: %j", (change) => {
  const { data, calls, client } = fixture();
  data[PULL] = { ...table(data[PULL]), ...change };
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow();
  expect(calls).toEqual([PULL]);
});

test.each([
  "created_at",
  "head_sha",
  "event",
  "path",
  "repository",
  "head_repository",
  "pull_requests",
  "status",
  "conclusion",
])("invalid or missing current-head normal CI identity refuses: %s", (key) => {
  const { data, client } = fixture();
  const candidate = run(20, "ci.yml");
  delete candidate[key];
  data[query("ci.yml")] = { workflow_runs: [candidate] };
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow();
});

test.each(["queued", "in_progress", "failure", "cancelled", "skipped"])(
  "newest %s run cannot fall back to an older green run",
  (state) => {
    const { data, client } = fixture();
    data[query("ci.yml")] = {
      workflow_runs: [
        run(20, "ci.yml"),
        { ...run(22, "ci.yml"), status: state, conclusion: state },
      ],
    };
    expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow();
  },
);

test.each(["2026-11-23T00:30:00Z", READY])(
  "old Ready run, late completion or rerun cannot become fresh evidence (%s)",
  (created_at) => {
    const { data, client } = fixture();
    data[TIMELINE] = [
      { event: "ready_for_review", created_at: "2026-11-23T00:00:00Z" },
      { event: "converted_to_draft", created_at: "2026-11-23T00:45:00Z" },
      { event: "ready_for_review", created_at: READY },
    ];
    data[query("ci.yml")] = {
      workflow_runs: [{ ...run(20, "ci.yml"), created_at, run_attempt: 2 }],
    };
    expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow("after the latest Ready");
  },
);

test("an unregistered Ready run refuses, even if the old draft workflow is green", () => {
  const { data, client } = fixture();
  data[query("ci.yml")] = { workflow_runs: [] };
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow("not registered");
});

test.each(["plan", "ci", STRICT_AGGREGATE_STEP])(
  "missing, skipped or failed %s proof refuses",
  (name) => {
    for (const conclusion of ["missing", "skipped", "failure", "cancelled"]) {
      const { data, client } = fixture();
      const jobs = [job("plan"), { ...job("ci"), steps: [job(STRICT_AGGREGATE_STEP)] }];
      if (name === STRICT_AGGREGATE_STEP)
        jobs[1] = {
          ...job("ci"),
          steps: conclusion === "missing" ? [] : [{ ...job(name), conclusion }],
        };
      else {
        const index = jobs.findIndex((candidate) => candidate["name"] === name);
        if (conclusion === "missing") jobs.splice(index, 1);
        else jobs[index] = { ...jobs[index], conclusion };
      }
      data[`${PREFIX}/actions/runs/20/attempts/1/jobs?per_page=100&page=1`] = { jobs };
      expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow();
    }
  },
);

test("a draft-only success and an older weak aggregate are never qualification", () => {
  for (const aggregate of [job("ci (draft)"), job("ci")]) {
    const { data, client } = fixture();
    data[`${PREFIX}/actions/runs/20/attempts/1/jobs?per_page=100&page=1`] = {
      jobs: [
        job("plan"),
        { ...aggregate, steps: [job("plan succeeded; every lane passed or was not needed")] },
      ],
    };
    expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow();
  }
});

test.each(["missing", "skipped", "failure", "cancelled", "in_progress"])(
  "current-head commitlint must genuinely succeed: %s",
  (conclusion) => {
    const { data, client } = fixture();
    data[`${PREFIX}/actions/runs/21/attempts/1/jobs?per_page=100&page=1`] = {
      jobs: conclusion === "missing" ? [] : [{ ...job("commitlint"), conclusion }],
    };
    expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow("commitlint");
  },
);

test("initially Ready PR uses creation; legacy pending is not cleared or mistaken for absence", () => {
  const { data, client } = fixture();
  data[TIMELINE] = [];
  expect(prReadyCi(REPO, 7, SHA, client)).toBe(0);
  data[STATUSES] = { statuses: [{ context: "ci", state: "pending" }] };
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow("legacy ci");
});

test.each(["head", "base", "draft", "updated_at", "timeline", "ci-run", "lint-run"])(
  "final rereads reject concurrent changes: %s",
  (race) => {
    const { data, client } = fixture();
    const baseJson = client.json.bind(client);
    const counts = new Map<string, number>();
    client.json = (path) => {
      const count = (counts.get(path) ?? 0) + 1;
      counts.set(path, count);
      if (path === PULL && count === 2) {
        const changes: Record<string, unknown> = {
          head: { head: { sha: "b".repeat(40), repo: { full_name: REPO } } },
          base: { base: { ref: "release", repo: { full_name: REPO } } },
          draft: { draft: true },
          updated_at: { updated_at: LATER },
        };
        data[PULL] = { ...table(data[PULL]), ...table(changes[race]) };
      }
      if (race === "timeline" && path === TIMELINE && count === 2) {
        data[TIMELINE] = [{ event: "converted_to_draft", created_at: LATER }];
      }
      const workflow = race === "ci-run" ? "ci.yml" : "commitlint.yml";
      if (race.endsWith("-run") && path === query(workflow) && count === 2) {
        data[path] = { workflow_runs: [{ ...run(99, workflow), status: "queued" }] };
      }
      return baseJson(path);
    };
    expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow();
  },
);

test("timeline pagination is complete and truncated/malformed evidence fails closed", () => {
  const { data, client, calls } = fixture();
  data[TIMELINE] = Array.from({ length: 100 }, () => ({ event: "commented" }));
  data[TIMELINE.replace("&page=1", "&page=2")] = [{ event: "ready_for_review", created_at: READY }];
  expect(prReadyCi(REPO, 7, SHA, client)).toBe(0);
  expect(calls).toContain(TIMELINE.replace("&page=1", "&page=2"));
  data[TIMELINE] = {};
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow("malformed");
});

test.each([
  ["ci", "pending"],
  ["ci", "failure"],
  ["commitlint", "pending"],
  ["commitlint", "failure"],
] as const)("legacy %s (%s) beyond page one refuses", (context, state) => {
  const { data, client, calls } = fixture();
  data[STATUSES] = {
    statuses: Array.from({ length: 100 }, (_, index) => ({
      context: `other-${index}`,
      state: "success",
    })),
  };
  const second = STATUSES.replace("&page=1", "&page=2");
  data[second] = { statuses: [{ context, state }] };
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow(`legacy ${context}`);
  expect(calls).toContain(second);
});

test("status pagination refuses truncation at its bound or malformed pages", () => {
  const { data, client } = fixture();
  for (let page = 1; page <= 20; page += 1) {
    data[STATUSES.replace("&page=1", `&page=${page}`)] = {
      statuses: Array.from({ length: 100 }, () => ({ context: "other", state: "success" })),
    };
  }
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow("pagination bound");
  data[STATUSES] = { total_count: 101 };
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow("malformed");
});

test("a genuine same-second Ready run stays refused with an actionable diagnostic", () => {
  const { data, client } = fixture();
  data[query("ci.yml")] = { workflow_runs: [{ ...run(20, "ci.yml"), created_at: READY }] };
  expect(() => prReadyCi(REPO, 7, SHA, client)).toThrow("Keep auto-merge off");
});

test("real CLI uses only read-only gh api calls; usage and help do not call gh", () => {
  const { data } = fixture();
  const responses = join(scratch, "responses.json");
  const log = join(scratch, "gh.log");
  const mock = join(scratch, "fake-gh.ts");
  writeFileSync(responses, JSON.stringify(data));
  writeFileSync(
    mock,
    `
import { appendFileSync, readFileSync } from "node:fs";
const args = process.argv.slice(2);
appendFileSync(process.env["REQUEST_LOG"]!, JSON.stringify(args) + "\\n");
if (args.length !== 2 || args[0] !== "api") process.exit(96);
const replies = JSON.parse(readFileSync(process.env["RESPONSES"]!, "utf8"));
if (!(args[1]! in replies)) process.exit(97);
console.log(JSON.stringify(replies[args[1]!]));
`,
  );
  writeFileSync(join(scratch, "gh"), `#!/bin/sh\nexec "${process.execPath}" "${mock}" "$@"\n`, {
    mode: 0o755,
  });
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => !key.startsWith("PHUX_")),
  );
  Object.assign(env, {
    PATH: `${scratch}:${env["PATH"] ?? ""}`,
    GITHUB_REPOSITORY: REPO,
    RESPONSES: responses,
    REQUEST_LOG: log,
  });
  const cli = (args: string[]) =>
    Bun.spawnSync([process.execPath, join(ROOT, "tools/main.ts"), "pr-ready-ci", ...args], {
      env,
      stdout: "pipe",
      stderr: "pipe",
    });
  expect(cli(["7", SHA]).exitCode).toBe(0);
  const requests = readFileSync(log, "utf8");
  expect(requests).not.toMatch(/rerun|merge|ready|--method|POST|PATCH|DELETE/);
  expect(cli(["--help"]).exitCode).toBe(0);
  expect(cli(["7", "short"]).exitCode).toBe(2);
  expect(cli(["7", SHA, "extra"]).exitCode).toBe(2);
  expect(readFileSync(log, "utf8")).toBe(requests);
  data[PULL] = { ...table(data[PULL]), draft: true };
  writeFileSync(responses, JSON.stringify(data));
  expect(cli(["7", SHA]).exitCode).toBe(1);
});
