/**
 * Budget gate for `just desktop-perf-headless` output (one JSON line per phase).
 *
 *   bun clients/desktop/scripts/perf-budget.ts <headless.jsonl>
 *
 * Scene and runtime counters are deterministic work budgets and must match
 * exactly: a cached redraw that stops reusing its scene, or a scroll that
 * reshapes every row, fails. Tick and RSS ceilings are catastrophe guards
 * loose enough for a loaded hosted runner and a debug build; the measured
 * times are retained as evidence, not compared between runs. A phase that is
 * missing, duplicated or unbudgeted fails, so the benchmark and this table
 * change together.
 */
import { readFileSync } from "node:fs";

export interface PhaseBudget {
  /** Exact values keyed by the JSON path under the phase line. */
  counters: Record<string, number>;
  tickMeanMaxUs: number;
}

const MAX_RSS_BYTES = 1024 * 1024 * 1024;

function counters(
  frames: number,
  reused: number,
  patched: number,
  shaped: number,
  published: number,
  fresh: number,
): Record<string, number> {
  return {
    frames,
    "scene.reused": reused,
    "scene.patched": patched,
    "scene.shaped_lines": shaped,
    "runtime.published": published,
    "runtime.buffer_fresh": fresh,
  };
}

export const BUDGETS: Record<string, PhaseBudget> = {
  cached: { counters: counters(128, 128, 0, 0, 0, 0), tickMeanMaxUs: 20_000 },
  sparse: { counters: counters(128, 0, 128, 128, 128, 1), tickMeanMaxUs: 60_000 },
  scroll: { counters: counters(128, 0, 128, 128, 128, 0), tickMeanMaxUs: 60_000 },
  wide_cached: { counters: counters(64, 64, 0, 0, 0, 0), tickMeanMaxUs: 60_000 },
  wide_redraw: { counters: counters(64, 0, 0, 3840, 64, 1), tickMeanMaxUs: 600_000 },
  wide_scroll: { counters: counters(64, 0, 64, 64, 64, 0), tickMeanMaxUs: 200_000 },
};

function lookup(value: unknown, path: string): unknown {
  let current = value;
  for (const key of path.split(".")) {
    if (current === null || typeof current !== "object" || Array.isArray(current)) return undefined;
    current = Object.entries(current).find(([name]) => name === key)?.[1];
  }
  return current;
}

function phaseLines(output: string): unknown[] {
  return output
    .split("\n")
    .filter((line) => line.startsWith("{"))
    .map((line): unknown => JSON.parse(line))
    .filter((line) => typeof lookup(line, "phase") === "string");
}

function checkPhase(name: string, line: unknown, budget: PhaseBudget): string[] {
  const failures: string[] = [];
  for (const [path, expected] of Object.entries(budget.counters)) {
    const actual = lookup(line, path);
    if (actual !== expected)
      failures.push(`${name}: ${path} is ${String(actual)}, budget ${expected}`);
  }
  const tick = lookup(line, "tick_us.mean");
  if (typeof tick !== "number" || tick > budget.tickMeanMaxUs) {
    failures.push(`${name}: tick_us.mean is ${String(tick)}, ceiling ${budget.tickMeanMaxUs}`);
  }
  const rss = lookup(line, "process.max_rss_bytes");
  if (typeof rss !== "number" || rss > MAX_RSS_BYTES) {
    failures.push(`${name}: process.max_rss_bytes is ${String(rss)}, ceiling ${MAX_RSS_BYTES}`);
  }
  return failures;
}

/** Every budget violation in `output`; empty when the run is within budget. */
export function checkPerfBudget(
  output: string,
  budgets: Record<string, PhaseBudget> = BUDGETS,
): string[] {
  const failures: string[] = [];
  const seen = new Set<string>();
  for (const line of phaseLines(output)) {
    const name = String(lookup(line, "phase"));
    const budget = Object.entries(budgets).find(([phase]) => phase === name)?.[1];
    if (!budget) {
      failures.push(`${name}: phase has no budget`);
      continue;
    }
    if (seen.has(name)) failures.push(`${name}: phase reported twice`);
    seen.add(name);
    failures.push(...checkPhase(name, line, budget));
  }
  for (const name of Object.keys(budgets)) {
    if (!seen.has(name)) failures.push(`${name}: phase missing from the run`);
  }
  return failures;
}

if (import.meta.main) {
  const path = process.argv[2];
  if (!path) {
    console.error("usage: bun clients/desktop/scripts/perf-budget.ts <headless.jsonl>");
    process.exit(2);
  }
  const failures = checkPerfBudget(readFileSync(path, "utf8"));
  for (const failure of failures) console.error(`budget: ${failure}`);
  if (failures.length > 0) process.exit(1);
  console.log(`desktop headless perf within budget (${Object.keys(BUDGETS).length} phases)`);
}
