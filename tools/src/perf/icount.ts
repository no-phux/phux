// Compare callgrind instruction counts to benchmarks/icount-baseline.json.
//
// A scenario fails when its count rises by more than `increase_limit_percent`;
// a drop passes; a missing or extra scenario fails. On a pull request, editing
// the baseline also needs the `Perf-Baseline: update` trailer or the
// `perf-baseline` label. Counts stay far below 2^53, so `number` is exact.
import { readFileSync, writeFileSync } from "node:fs";
import { Failure } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { ROOT, repoPath } from "../lib/repo.ts";
import { isTable, list, str, table, type Table } from "../lib/value.ts";

export const BASELINE = "benchmarks/icount-baseline.json";
const TRAILER = "Perf-Baseline: update";

export type Measurements = Record<string, number>;

export interface Baseline {
  readonly increase_limit_percent: number;
  readonly scenarios: Measurements;
}

const isCount = (value: unknown): value is number => Number.isInteger(value);

/** The current run's Callgrind Ir total: `Left`, or the first of `Both`. */
export function instructionCount(summary: Table): number {
  for (const profile of list(summary["profiles"]).map(table)) {
    if (profile["tool"] !== undefined && profile["tool"] !== "Callgrind") continue;
    const callgrind = table(
      table(table(table(profile["summaries"])["total"])["summary"])["Callgrind"],
    );
    const metrics = table(table(callgrind["Ir"])["metrics"]);
    const current = table("Left" in metrics ? metrics["Left"] : list(metrics["Both"])[0]);
    if (isCount(current["Int"])) return current["Int"];
  }
  throw new Failure(`no integer Callgrind Ir total for ${String(summary["function_name"])}`);
}

/** One scenario per iai-callgrind JSON line, named `function/id`. */
export function loadMeasurements(text: string): Measurements {
  const found: Measurements = {};
  for (const [index, line] of text.split("\n").entries()) {
    if (line.trim() === "") continue;
    let summary: unknown;
    try {
      summary = JSON.parse(line);
    } catch {
      throw new Failure(`iai json line ${index + 1} is not JSON`);
    }
    const fn = isTable(summary) ? str(summary["function_name"]) : undefined;
    if (!isTable(summary) || !fn) continue;
    const id = str(summary["id"]);
    const name = id ? `${fn}/${id}` : fn;
    if (name in found) throw new Failure(`duplicate scenario ${name}`);
    found[name] = instructionCount(summary);
  }
  if (Object.keys(found).length === 0) throw new Failure("iai output has no benchmark summaries");
  return found;
}

export function parseBaseline(text: string): Baseline {
  const data = table(JSON.parse(text));
  if (data["schema"] !== 1 || data["tool"] !== "callgrind" || data["profile"] !== "icount") {
    throw new Failure(`${BASELINE} must be schema 1 of the callgrind icount profile`);
  }
  const limit = data["increase_limit_percent"];
  if (!isCount(limit) || limit < 0) {
    throw new Failure("increase_limit_percent must be a non-negative integer");
  }
  const scenarios = table(data["scenarios"]);
  if (Object.keys(scenarios).length === 0)
    throw new Failure("baseline scenarios must not be empty");
  for (const [name, count] of Object.entries(scenarios)) {
    if (!isCount(count) || count < 0) {
      throw new Failure(`scenario ${name} must map to a non-negative integer`);
    }
  }
  return { increase_limit_percent: limit, scenarios: scenarios as Measurements };
}

export function loadBaseline(): Baseline {
  let text: string;
  try {
    text = readFileSync(repoPath(BASELINE), "utf8");
  } catch {
    throw new Failure(
      `cannot read ${BASELINE}. Run \`just perf-icount --update\` on x86_64 inside nix develop and commit it.`,
    );
  }
  return parseBaseline(text);
}

export function compare(
  measured: Measurements,
  baseline: Baseline,
): { lines: string[]; failures: string[] } {
  const { increase_limit_percent: limit, scenarios } = baseline;
  const want = Object.keys(scenarios).sort();
  const failures = [
    ...want
      .filter((name) => !(name in measured))
      .map((name) => `FAIL ${name}: missing from this run`),
    ...Object.keys(measured)
      .sort()
      .filter((name) => !(name in scenarios))
      .map((name) => `FAIL ${name}: not in the baseline`),
  ];
  const lines = want
    .filter((name) => name in measured)
    .map((name) => {
      const count = measured[name] ?? 0;
      const base = scenarios[name] ?? 0;
      const change =
        base === 0
          ? "n/a"
          : `${count >= base ? "+" : ""}${(((count - base) * 100) / base).toFixed(2)}%`;
      const over = count * 100 > base * (100 + limit);
      const line = `${over ? "FAIL" : "ok"} ${name}: measured ${count} baseline ${base} (${change}, limit +${limit}%)`;
      if (over) failures.push(line);
      return line;
    });
  return { lines, failures };
}

export function baselineDocument(measured: Measurements): string {
  const scenarios = Object.fromEntries(
    Object.keys(measured)
      .sort()
      .map((name) => [name, measured[name]]),
  );
  const document = {
    schema: 1,
    profile: "icount",
    tool: "callgrind",
    increase_limit_percent: 5,
    scenarios,
  };
  return `${JSON.stringify(document, null, 2)}\n`;
}

export function writeBaseline(measured: Measurements): void {
  writeFileSync(repoPath(BASELINE), baselineDocument(measured));
}

function baselineChanged(base: string, cwd: string): boolean {
  const diff = Bun.spawnSync(["git", "diff", "--quiet", `${base}...HEAD`, "--", BASELINE], { cwd });
  if (diff.exitCode === 0 || diff.exitCode === 1) return diff.exitCode === 1;
  throw new Failure(
    `git diff against ${base} failed: ${diff.stderr.toString().trim()}. Fetch the pull request base first.`,
  );
}

/**
 * Refuse a pull request that edits the baseline without the trailer or the
 * label. Returns the authorization notice to print, if any.
 */
export function authorize(
  base: string,
  event: string,
  labeled: boolean,
  cwd = ROOT,
): string | undefined {
  if (event !== "pull_request") return undefined;
  if (base === "") throw new Failure("PHUX_BASE_SHA is required on pull_request");
  if (!baselineChanged(base, cwd)) return undefined;
  if (labeled) return "perf-icount: baseline change authorized by the perf-baseline label";
  if (
    git(["log", "--format=%B", `${base}..HEAD`], cwd)
      .split("\n")
      .includes(TRAILER)
  ) {
    return `perf-icount: baseline change authorized by ${TRAILER}`;
  }
  throw new Failure(
    `${BASELINE} changed without authorization. Add a commit with a line \`${TRAILER}\`, ` +
      "or add the `perf-baseline` label and push again (labeling alone does not start ci).",
  );
}

/** The iai-callgrind version Cargo.lock pins; iai-callgrind-runner must match it. */
export function runnerVersion(lockText: string): string {
  const versions = list(table(Bun.TOML.parse(lockText))["package"])
    .map(table)
    .filter((pkg) => pkg["name"] === "iai-callgrind")
    .map((pkg) => str(pkg["version"]));
  const [version] = versions;
  if (versions.length !== 1 || version === undefined) {
    throw new Failure("Cargo.lock must contain exactly one iai-callgrind");
  }
  return version;
}
