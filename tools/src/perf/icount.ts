// Compare callgrind instruction counts to benchmarks/icount-baseline.json.
//
// `scripts/ci/perf-icount.sh` runs the bench and calls this. The checked-in
// file is the contract: a scenario fails when its instruction count rises by
// more than `increase_limit_percent`. A drop passes. Missing or extra
// scenarios fail. `update` rewrites the scenario map from the measurement.
//
// On a pull request, a change to the baseline file also needs the commit
// trailer `Perf-Baseline: update` or `PHUX_PERF_BASELINE_UPDATE=1` (the
// `perf-baseline` label). That check authorizes the edit; it does not skip
// the percentage comparison.
//
// Counts are JSON numbers. Callgrind totals for these scenarios are far below
// 2^53, and so are their products with 100 + limit, so integer arithmetic on
// `number` stays exact.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, relative } from "node:path";
import { GitError, git } from "../lib/git.ts";
import { ROOT, repoPath } from "../lib/repo.ts";
import { isTable, list, str, table, type Table } from "../lib/value.ts";

export const BASELINE_PATH = repoPath("benchmarks/icount-baseline.json");
const BASELINE_FILE = "benchmarks/icount-baseline.json";
export const TRAILER = "Perf-Baseline: update";
const SCHEMA = {
  schema: 1,
  profile: "icount",
  tool: "callgrind",
  increase_limit_percent: 5,
} as const;

export type Measurements = Record<string, number>;

export interface Baseline {
  readonly increase_limit_percent: number;
  readonly scenarios: Measurements;
}

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value);
}

function repr(value: unknown): string {
  return JSON.stringify(value) ?? String(value);
}

export function scenarioName(summary: Table): string {
  const fn = summary["function_name"];
  if (typeof fn !== "string" || fn === "") {
    throw new Error("benchmark summary has no function_name");
  }
  const id = summary["id"];
  return typeof id === "string" && id !== "" ? `${fn}/${id}` : fn;
}

function intMetric(node: unknown): number {
  if (isTable(node) && "Int" in node) {
    const value = node["Int"];
    if (isCount(value)) return value;
    if (typeof value === "string" && /^[+-]?\d+$/.test(value.trim())) return Number(value);
  } else if (isCount(node)) {
    return node;
  }
  throw new Error(`instruction metric is not an integer: ${repr(node)}`);
}

/** The current run is Left, or the first element of Both. */
function metricNew(metrics: unknown): number {
  if (!isTable(metrics)) throw new Error(`metrics object missing: ${repr(metrics)}`);
  if ("Left" in metrics) return intMetric(metrics["Left"]);
  if ("Both" in metrics) {
    const pair = metrics["Both"];
    if (!Array.isArray(pair) || pair.length === 0) {
      throw new Error(`Both metrics are empty: ${repr(metrics)}`);
    }
    return intMetric(pair[0]);
  }
  throw new Error(`no new instruction metric in ${repr(metrics)}`);
}

export function instructionCount(summary: Table): number {
  const profiles = summary["profiles"];
  if (!Array.isArray(profiles)) throw new Error("benchmark summary has no profiles list");
  for (const profile of list(profiles)) {
    if (!isTable(profile)) continue;
    const tool = profile["tool"];
    if (tool !== undefined && tool !== null && tool !== "Callgrind") continue;
    const total = table(table(profile["summaries"])["total"]);
    const callgrind = table(total["summary"])["Callgrind"];
    if (!isTable(callgrind) || !("Ir" in callgrind)) continue;
    return metricNew(table(callgrind["Ir"])["metrics"]);
  }
  const keys = Object.keys(summary).sort();
  throw new Error(`no Callgrind Ir total in summary (keys=${repr(keys)})`);
}

/** One scenario per iai-callgrind JSON line; other JSON lines are ignored. */
export function loadMeasurements(text: string): Measurements {
  const found: Measurements = {};
  const lines = text.split(/\r\n|\r|\n/);
  for (const [index, raw] of lines.entries()) {
    const line = raw.trim();
    if (line === "") continue;
    let summary: unknown;
    try {
      summary = JSON.parse(line);
    } catch (error) {
      throw new Error(`iai json line ${index + 1} is not JSON: ${String(error)}`, {
        cause: error,
      });
    }
    if (!isTable(summary) || !("function_name" in summary)) continue;
    const name = scenarioName(summary);
    if (name in found) throw new Error(`duplicate scenario ${name}`);
    found[name] = instructionCount(summary);
  }
  if (Object.keys(found).length === 0) throw new Error("iai output has no benchmark summaries");
  return found;
}

export function parseBaseline(text: string, path: string): Baseline {
  const data: unknown = JSON.parse(text);
  if (!isTable(data)) throw new Error(`${path} must hold a JSON object`);
  if (data["schema"] !== 1) throw new Error(`${path} schema must be 1`);
  if (data["tool"] !== "callgrind" || data["profile"] !== "icount") {
    throw new Error(`${path} must record the icount profile under callgrind`);
  }
  const limit = data["increase_limit_percent"];
  if (!isCount(limit) || limit < 0) {
    throw new Error("increase_limit_percent must be a non-negative integer");
  }
  const scenarios = data["scenarios"];
  if (!isTable(scenarios) || Object.keys(scenarios).length === 0) {
    throw new Error("baseline scenarios must be a non-empty object");
  }
  const parsed: Measurements = {};
  for (const [name, count] of Object.entries(scenarios)) {
    if (!isCount(count)) {
      throw new Error(`scenario ${repr(name)} must map to an integer instruction count`);
    }
    if (count < 0) throw new Error(`scenario ${name} has a negative count`);
    parsed[name] = count;
  }
  return { increase_limit_percent: limit, scenarios: parsed };
}

export function loadBaseline(path: string): Baseline {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (error) {
    if (isMissing(error)) {
      throw new Error(
        `missing ${path}. Run \`just perf-icount --update\` on x86_64 ` +
          "inside nix develop and commit the file.",
        { cause: error },
      );
    }
    throw error;
  }
  return parseBaseline(text, path);
}

function isMissing(error: unknown): boolean {
  return isTable(error) && error["code"] === "ENOENT";
}

/** True when the increase is strictly more than limitPercent. */
export function overLimit(measured: number, baseline: number, limitPercent: number): boolean {
  return measured * 100 > baseline * (100 + limitPercent);
}

function percent(got: number, want: number): string {
  const change = ((got - want) * 100) / want;
  return `${change >= 0 ? "+" : ""}${change.toFixed(2)}%`;
}

export interface Comparison {
  /** One line per scenario on both sides, ok or FAIL. */
  readonly lines: string[];
  /** Missing, extra, and over-limit scenarios. */
  readonly failures: string[];
}

export function compare(measured: Measurements, baseline: Baseline): Comparison {
  const limit = baseline.increase_limit_percent;
  const expected = baseline.scenarios;
  const lines: string[] = [];
  const failures: string[] = [];
  // Default sort: code-unit order, which is what Python's sorted() gives these names.
  const want = Object.keys(expected).sort();
  const got = Object.keys(measured).sort();
  for (const name of want.filter((name) => !(name in measured))) {
    failures.push(`FAIL ${name}: missing from this run`);
  }
  for (const name of got.filter((name) => !(name in expected))) {
    failures.push(`FAIL ${name}: not in the baseline`);
  }
  for (const name of want.filter((name) => name in measured)) {
    const count = measured[name] ?? 0;
    const base = expected[name] ?? 0;
    const pct = base === 0 ? "n/a" : percent(count, base);
    const bad = base === 0 ? count > 0 : overLimit(count, base, limit);
    const line = `${bad ? "FAIL" : "ok"} ${name}: measured ${count} baseline ${base} (${pct}, limit +${limit}%)`;
    lines.push(line);
    if (bad) failures.push(line);
  }
  return { lines, failures };
}

export function baselineDocument(measured: Measurements): string {
  const scenarios = Object.fromEntries(
    Object.keys(measured)
      .sort()
      .map((name) => [name, measured[name]]),
  );
  return `${JSON.stringify({ ...SCHEMA, scenarios }, null, 2)}\n`;
}

export function writeBaseline(path: string, measured: Measurements): string {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, baselineDocument(measured));
  const shown = relative(ROOT, path);
  return `wrote ${shown.startsWith("..") || isAbsolute(shown) ? path : shown}`;
}

export function trailerPresent(message: string): boolean {
  return message.split(/\r\n|\r|\n/).some((line) => line === TRAILER);
}

function baselineDiffers(base: string, cwd: string): boolean {
  const result = Bun.spawnSync(["git", "diff", "--quiet", `${base}...HEAD`, "--", BASELINE_FILE], {
    cwd,
    stdout: "ignore",
    stderr: "pipe",
  });
  if (result.exitCode === 0) return false;
  if (result.exitCode === 1) return true;
  throw new Error(
    `git diff against ${base} failed (exit ${result.exitCode}: ${result.stderr.toString().trim()}). ` +
      "Fetch the pull request base before running the guard.",
  );
}

/**
 * Refuse a pull request that edits the baseline without the trailer or the
 * label. Returns the authorization notice to print, if any.
 */
export function authorize(
  base: string,
  event: string,
  labelUpdate: string,
  cwd = ROOT,
): string | undefined {
  if (event !== "pull_request") return undefined;
  if (base === "") throw new Error("PHUX_BASE_SHA is required on pull_request");
  if (!baselineDiffers(base, cwd)) return undefined;
  if (labelUpdate === "1") {
    return "perf-icount: baseline change authorized by the perf-baseline label";
  }
  let message: string;
  try {
    message = git(["log", "--format=%B", `${base}..HEAD`], cwd);
  } catch (error) {
    if (error instanceof GitError) throw new Error(error.message, { cause: error });
    throw error;
  }
  if (trailerPresent(message)) {
    return `perf-icount: baseline change authorized by ${TRAILER}`;
  }
  throw new Error(
    `${BASELINE_FILE} changed without authorization. ` +
      `Add a commit whose message contains a line \`${TRAILER}\`, ` +
      "or push again after adding the `perf-baseline` label. " +
      "The label is read on the next pull_request run; labeling alone " +
      "does not start ci.",
  );
}

/**
 * The iai-callgrind version Cargo.lock pins. The bench spawns
 * iai-callgrind-runner, whose version has to match the library's.
 */
export function runnerVersion(lockText: string): string {
  const lock = table(Bun.TOML.parse(lockText));
  const versions = list(lock["package"])
    .map(table)
    .filter((pkg) => pkg["name"] === "iai-callgrind")
    .map((pkg) => str(pkg["version"]));
  const [version] = versions;
  if (versions.length !== 1 || version === undefined) {
    throw new Error("Cargo.lock must contain exactly one iai-callgrind");
  }
  return version;
}
