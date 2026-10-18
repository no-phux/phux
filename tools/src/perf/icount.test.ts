import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { Classifier } from "../routing/classify.ts";
import { repoPath } from "../lib/repo.ts";
import {
  authorize,
  baselineDocument,
  compare,
  instructionCount,
  loadMeasurements,
  overLimit,
  parseBaseline,
  runnerVersion,
  trailerPresent,
} from "./icount.ts";

function summary(name: string, id: string, instructions: number): Record<string, unknown> {
  return {
    function_name: name,
    id,
    profiles: [
      {
        tool: "Callgrind",
        summaries: {
          total: {
            summary: {
              Callgrind: { Ir: { diffs: null, metrics: { Left: { Int: instructions } } } },
            },
          },
        },
      },
    ],
  };
}

describe("extract", () => {
  test("Left and Both keep the new count", () => {
    expect(instructionCount(summary("vt_apply", "apply", 1000))).toBe(1000);
    const both = summary("vt_apply", "apply", 1);
    both["profiles"] = [
      {
        tool: "Callgrind",
        summaries: {
          total: {
            summary: { Callgrind: { Ir: { metrics: { Both: [{ Int: 1100 }, { Int: 1000 }] } } } },
          },
        },
      },
    ];
    expect(instructionCount(both)).toBe(1100);
  });

  test("load names scenarios by function and id", () => {
    const text = [
      JSON.stringify(summary("vt_apply", "apply", 10)),
      JSON.stringify(summary("frame_encode", "encode", 20)),
      "not a summary",
    ].join("\n");
    // The third line is JSON-invalid and must fail, not be skipped.
    expect(() => loadMeasurements(`${text}\n{`)).toThrow();
    expect(loadMeasurements(text.replace("not a summary", ""))).toEqual({
      "vt_apply/apply": 10,
      "frame_encode/encode": 20,
    });
  });

  test("a summary without an id is named by its function", () => {
    const line = JSON.stringify({ ...summary("org_commit", "", 7) });
    expect(loadMeasurements(line)).toEqual({ org_commit: 7 });
    expect(() => loadMeasurements(`${line}\n${line}`)).toThrow("duplicate scenario org_commit");
    expect(() => loadMeasurements('{"other": 1}')).toThrow("no benchmark summaries");
  });
});

describe("limit", () => {
  test("exactly five percent passes and one more fails", () => {
    expect(overLimit(105, 100, 5)).toBe(false);
    expect(overLimit(106, 100, 5)).toBe(true);
    expect(overLimit(90, 100, 5)).toBe(false);
  });

  test("compare reports missing and extra", () => {
    const { failures } = compare(
      { a: 100, extra: 1 },
      { increase_limit_percent: 5, scenarios: { a: 100, missing: 1 } },
    );
    expect(failures).toHaveLength(2);
    expect(failures.some((line) => line.includes("missing"))).toBe(true);
    expect(failures.some((line) => line.includes("extra"))).toBe(true);
  });

  test("compare prints the change and the limit", () => {
    const { lines, failures } = compare(
      { a: 106, b: 90, zero: 0 },
      { increase_limit_percent: 5, scenarios: { a: 100, b: 100, zero: 0 } },
    );
    expect(lines).toEqual([
      "FAIL a: measured 106 baseline 100 (+6.00%, limit +5%)",
      "ok b: measured 90 baseline 100 (-10.00%, limit +5%)",
      "ok zero: measured 0 baseline 0 (n/a, limit +5%)",
    ]);
    expect(failures).toEqual(lines.slice(0, 1));
  });
});

describe("baseline", () => {
  test("the checked-in baseline parses and round-trips byte for byte", () => {
    const text = readFileSync(repoPath("benchmarks/icount-baseline.json"), "utf8");
    const baseline = parseBaseline(text, "baseline");
    expect(baselineDocument(baseline.scenarios)).toBe(text);
  });

  test("malformed baselines are refused", () => {
    const valid = { schema: 1, profile: "icount", tool: "callgrind", increase_limit_percent: 5 };
    const cases: [unknown, string][] = [
      [{ ...valid, schema: 2, scenarios: { a: 1 } }, "schema must be 1"],
      [{ ...valid, tool: "cachegrind", scenarios: { a: 1 } }, "icount profile under callgrind"],
      [{ ...valid, increase_limit_percent: -1, scenarios: { a: 1 } }, "non-negative integer"],
      [{ ...valid, increase_limit_percent: true, scenarios: { a: 1 } }, "non-negative integer"],
      [{ ...valid, scenarios: {} }, "non-empty object"],
      [{ ...valid, scenarios: { a: 1.5 } }, "integer instruction count"],
      [{ ...valid, scenarios: { a: -1 } }, "negative count"],
    ];
    for (const [document, message] of cases) {
      expect(() => parseBaseline(JSON.stringify(document), "b.json")).toThrow(message);
    }
  });
});

test("the trailer is a whole line", () => {
  expect(trailerPresent("subject\n\nPerf-Baseline: update\n")).toBe(true);
  expect(trailerPresent("Perf-Baseline: update now")).toBe(false);
  expect(trailerPresent("see Perf-Baseline: update\n")).toBe(false);
});

test("the runner version is the one iai-callgrind in Cargo.lock", () => {
  const lock = (versions: string[]): string =>
    versions
      .map((version) => `[[package]]\nname = "iai-callgrind"\nversion = "${version}"\n`)
      .join("\n");
  expect(runnerVersion(lock(["0.16.1"]))).toBe("0.16.1");
  expect(() => runnerVersion(lock([]))).toThrow("exactly one");
  expect(() => runnerVersion(lock(["0.16.1", "0.15.0"]))).toThrow("exactly one");
  expect(runnerVersion(readFileSync(repoPath("Cargo.lock"), "utf8"))).toMatch(/^\d+\.\d+\.\d+/);
});

test("hot paths select the job and docs do not", () => {
  const classifier = new Classifier();
  const cases: Record<string, boolean> = {
    "": true,
    "crates/phux-server/src/lib.rs": true,
    "crates/phux-protocol/src/lib.rs": true,
    "crates/phux-client-core/src/lib.rs": true,
    "crates/phux-core/src/org.rs": true,
    "benchmarks/icount-baseline.json": true,
    "just/perf.just": true,
    ".github/workflows/ci.yml": true,
    "tools/src/perf/icount.ts": true,
    "docs/performance.md": false,
    "crates/phux-tui/src/lib.rs": false,
    "scripts/bench/perf-nightly.sh": false,
  };
  for (const [path, want] of Object.entries(cases)) {
    const paths = path === "" ? [] : [path];
    expect([path, classifier.classify(paths).perfIcountNeeded]).toEqual([path, want]);
  }
});

describe("authorize", () => {
  function fixture(body: (repo: string, base: string) => void): void {
    const repo = mkdtempSync(join(tmpdir(), "phux-icount-"));
    try {
      git(["init", "-q"], repo);
      mkdirSync(join(repo, "benchmarks"));
      writeFileSync(join(repo, "benchmarks/icount-baseline.json"), "{}\n");
      writeFileSync(join(repo, "README"), "one\n");
      commitAll(repo, "base");
      const base = git(["rev-parse", "HEAD"], repo).trim();
      body(repo, base);
    } finally {
      rmSync(repo, { recursive: true, force: true });
    }
  }

  function commitAll(repo: string, message: string): void {
    git(["add", "-A"], repo);
    git(["-c", "user.name=t", "-c", "user.email=t@e", "commit", "-qm", message], repo);
  }

  test("only pull requests are checked, and they need a base", () => {
    expect(authorize("", "push", "0")).toBeUndefined();
    expect(() => authorize("", "pull_request", "0")).toThrow("PHUX_BASE_SHA is required");
  });

  test("an untouched baseline needs nothing", () => {
    fixture((repo, base) => {
      writeFileSync(join(repo, "README"), "two\n");
      commitAll(repo, "docs");
      expect(authorize(base, "pull_request", "0", repo)).toBeUndefined();
    });
  });

  test("a baseline edit needs the trailer or the label", () => {
    fixture((repo, base) => {
      writeFileSync(join(repo, "benchmarks/icount-baseline.json"), '{"x": 1}\n');
      commitAll(repo, "perf: move the baseline\n\nPerf-Baseline: update now");
      expect(() => authorize(base, "pull_request", "0", repo)).toThrow(
        "changed without authorization",
      );
      expect(authorize(base, "pull_request", "1", repo)).toContain("perf-baseline label");
      writeFileSync(join(repo, "README"), "two\n");
      commitAll(repo, "follow-up\n\nPerf-Baseline: update");
      expect(authorize(base, "pull_request", "0", repo)).toContain("by Perf-Baseline: update");
    });
  });

  test("an unreachable base fails closed", () => {
    fixture((repo) => {
      expect(() => authorize("0".repeat(40), "pull_request", "0", repo)).toThrow(
        "Fetch the pull request base",
      );
    });
  });
});
