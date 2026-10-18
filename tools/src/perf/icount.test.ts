import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { repoPath } from "../lib/repo.ts";
import { Classifier } from "../routing/classify.ts";
import {
  authorize,
  baselineDocument,
  compare,
  loadMeasurements,
  parseBaseline,
  runnerVersion,
} from "./icount.ts";

function line(name: string, id: string, metrics: unknown): string {
  const ir = { Ir: { metrics } };
  return JSON.stringify({
    function_name: name,
    id,
    profiles: [{ tool: "Callgrind", summaries: { total: { summary: { Callgrind: ir } } } }],
  });
}

test("measurements are named function/id and keep the new count", () => {
  const text = [
    line("vt_apply", "apply", { Left: { Int: 10 } }),
    line("frame_encode", "encode", { Both: [{ Int: 1100 }, { Int: 1000 }] }),
    line("org_commit", "", { Left: { Int: 7 } }),
    '{"other": 1}',
    "",
  ].join("\n");
  expect(loadMeasurements(text)).toEqual({
    "vt_apply/apply": 10,
    "frame_encode/encode": 1100,
    org_commit: 7,
  });
  const one = line("a", "", { Left: { Int: 1 } });
  expect(() => loadMeasurements(`${one}\n{`)).toThrow("line 2 is not JSON");
  expect(() => loadMeasurements(`${one}\n${one}`)).toThrow("duplicate scenario a");
  expect(() => loadMeasurements('{"other": 1}')).toThrow("no benchmark summaries");
  expect(() => loadMeasurements(line("a", "", { Left: { Float: 1.5 } }))).toThrow(
    "no integer Callgrind Ir",
  );
});

test("compare allows exactly the limit and fails missing, extra and over-limit scenarios", () => {
  const { lines, failures } = compare(
    { a: 105, b: 106, c: 90, zero: 0, extra: 1 },
    { increase_limit_percent: 5, scenarios: { a: 100, b: 100, c: 100, zero: 0, missing: 1 } },
  );
  expect(lines).toEqual([
    "ok a: measured 105 baseline 100 (+5.00%, limit +5%)",
    "FAIL b: measured 106 baseline 100 (+6.00%, limit +5%)",
    "ok c: measured 90 baseline 100 (-10.00%, limit +5%)",
    "ok zero: measured 0 baseline 0 (n/a, limit +5%)",
  ]);
  expect(failures).toEqual([
    "FAIL missing: missing from this run",
    "FAIL extra: not in the baseline",
    "FAIL b: measured 106 baseline 100 (+6.00%, limit +5%)",
  ]);
});

test("the checked-in baseline parses and round-trips byte for byte", () => {
  const text = readFileSync(repoPath("benchmarks/icount-baseline.json"), "utf8");
  expect(baselineDocument(parseBaseline(text).scenarios)).toBe(text);
});

test.each<[Record<string, unknown>, string]>([
  [{ schema: 2 }, "schema 1"],
  [{ tool: "cachegrind" }, "schema 1"],
  [{ increase_limit_percent: -1 }, "non-negative integer"],
  [{ increase_limit_percent: true }, "non-negative integer"],
  [{ scenarios: {} }, "must not be empty"],
  [{ scenarios: { a: 1.5 } }, "scenario a"],
  [{ scenarios: { a: -1 } }, "scenario a"],
])("baseline %p is refused", (change, message) => {
  const valid = {
    schema: 1,
    profile: "icount",
    tool: "callgrind",
    increase_limit_percent: 5,
    scenarios: { a: 1 },
  };
  expect(() => parseBaseline(JSON.stringify({ ...valid, ...change }))).toThrow(message);
});

test("the runner version is the one iai-callgrind in Cargo.lock", () => {
  const lock = (...versions: string[]): string =>
    versions
      .map((version) => `[[package]]\nname = "iai-callgrind"\nversion = "${version}"\n`)
      .join("\n");
  expect(runnerVersion(lock("0.16.1"))).toBe("0.16.1");
  expect(() => runnerVersion(lock())).toThrow("exactly one");
  expect(() => runnerVersion(lock("0.16.1", "0.15.0"))).toThrow("exactly one");
  expect(runnerVersion(readFileSync(repoPath("Cargo.lock"), "utf8"))).toMatch(/^\d+\.\d+\.\d+/);
});

test("hot paths select the job and docs do not", () => {
  const classifier = new Classifier();
  const cases: [string[], boolean][] = [
    [[], true],
    [["crates/phux-server/src/lib.rs"], true],
    [["benchmarks/icount-baseline.json"], true],
    [["tools/src/perf/icount.ts"], true],
    [["docs/performance.md"], false],
    [["crates/phux-tui/src/lib.rs"], false],
  ];
  for (const [paths, want] of cases) {
    expect([paths, classifier.classify(paths).perfIcountNeeded]).toEqual([paths, want]);
  }
});

test("a pull request baseline edit needs the trailer or the label", () => {
  expect(authorize("", "push", false)).toBeUndefined();
  expect(() => authorize("", "pull_request", false)).toThrow("PHUX_BASE_SHA is required");
  const repo = mkdtempSync(join(tmpdir(), "phux-icount-"));
  const commit = (path: string, text: string, message: string): void => {
    writeFileSync(join(repo, path), text);
    git(["add", "-A"], repo);
    git(["-c", "user.name=t", "-c", "user.email=t@e", "commit", "-qm", message], repo);
  };
  try {
    git(["init", "-q"], repo);
    mkdirSync(join(repo, "benchmarks"));
    commit("benchmarks/icount-baseline.json", "{}\n", "base");
    const base = git(["rev-parse", "HEAD"], repo).trim();
    expect(() => authorize("0".repeat(40), "pull_request", false, repo)).toThrow(
      "Fetch the pull request base",
    );
    commit("README", "docs\n", "docs");
    expect(authorize(base, "pull_request", false, repo)).toBeUndefined();
    commit(
      "benchmarks/icount-baseline.json",
      '{"x": 1}\n',
      "perf: move\n\nPerf-Baseline: update now",
    );
    expect(() => authorize(base, "pull_request", false, repo)).toThrow(
      "changed without authorization",
    );
    expect(authorize(base, "pull_request", true, repo)).toContain("perf-baseline label");
    commit("README", "two\n", "follow-up\n\nPerf-Baseline: update");
    expect(authorize(base, "pull_request", false, repo)).toContain("by Perf-Baseline: update");
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});
