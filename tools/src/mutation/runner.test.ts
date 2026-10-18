import { describe, expect, test } from "bun:test";
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { table } from "../lib/value.ts";
import { living, parseSnapshot, stopInvocation, trackDescendants } from "./processes.ts";
import { parseOptions } from "./runner.ts";

describe("parseOptions", () => {
  test("defaults are the bounded pilot; repeatable scopes keep their order", () => {
    expect(parseOptions([])).toMatchObject({ limit: 8, jobs: 1, list: false, root: ROOT });
    expect(parseOptions([])).toMatchObject({
      buildTimeout: 300,
      testTimeout: 60,
      runTimeout: 1800,
    });
    const options = parseOptions(["--file", "a.rs", "--package", "p", "--file", "b.rs"]);
    expect([options.file, options.package]).toEqual([["a.rs", "b.rs"], ["p"]]);
  });

  test.each([
    [["--limit", "0"], "must be at least 1"],
    [["--run-timeout", "2.5"], "invalid int value"],
    [["--jobs", "5"], "invalid choice"],
    [["--unknown"], "Unknown option"],
  ])("%p is rejected", (args, message) => {
    expect(() => parseOptions(args)).toThrow(message);
  });
});

test("descendants are adopted transitively, but not through a reused PID", () => {
  const snapshot = parseSnapshot("  10     1 T0\n  11    10 T1\n  12    11 T2\n  13     1 T3\n");
  const owned = new Map([[10, "T0"]]);
  trackDescendants(owned, snapshot);
  expect([...owned.keys()]).toEqual([10, 11, 12]);
  const reused = new Map([[10, "earlier"]]);
  trackDescendants(reused, snapshot);
  expect([living(reused, snapshot).size, reused.size]).toEqual([0, 1]);
});

// A stand-in cargo-mutants: ignores SIGTERM, exits on SIGINT only with
// FAKE_EXIT_ON_SIGINT, and starts FAKE_DEPTH generations of detached
// descendants that ignore both.
const FAKE_TOOL = `#!${process.execPath}
import { appendFileSync, writeFileSync, writeSync } from "node:fs";
const depth = Number(process.env.FAKE_DEPTH ?? 0);
process.on("SIGTERM", () => {});
process.on("SIGINT", () => { if (process.env.FAKE_EXIT_ON_SIGINT) process.exit(0); });
appendFileSync(import.meta.dir + "/pids", process.pid + "\\n");
writeSync(1, "partial diagnostic\\n");
const env = { ...process.env, FAKE_DEPTH: String(depth - 1), FAKE_EXIT_ON_SIGINT: "" };
if (depth > 0) Bun.spawn([import.meta.path], { env, detached: true, stdio: ["ignore", "ignore", "ignore"] }).unref();
else writeFileSync(import.meta.dir + "/ready", "");
setInterval(() => {}, 1000);
`;

/** Runs `body` with a fresh fake tool, then kills exactly the processes it started. */
async function withFakeTool(body: (dir: string, tool: string) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "phux-mutation-fake-"));
  const tool = join(dir, "cargo-mutants");
  writeFileSync(tool, FAKE_TOOL);
  chmodSync(tool, 0o755);
  try {
    await body(dir, tool);
  } finally {
    for (const pid of survivors(dir, tool)) process.kill(pid, "SIGKILL");
    rmSync(dir, { recursive: true, force: true });
  }
}

function pids(dir: string): number[] {
  const path = join(dir, "pids");
  return existsSync(path) ? readFileSync(path, "utf8").trim().split("\n").map(Number) : [];
}

function survivors(dir: string, tool: string): number[] {
  const command = (pid: number) => Bun.spawnSync(["ps", "-p", `${pid}`, "-o", "command="]).stdout;
  return pids(dir).filter((pid) => command(pid).toString().includes(tool));
}

async function eventually(condition: () => boolean): Promise<void> {
  const deadline = performance.now() + 5000;
  while (!condition() && performance.now() < deadline) await Bun.sleep(20);
  expect(condition()).toBe(true);
}

test.each([
  ["exits on SIGINT, orphaning its descendants", "1", 2],
  ["ignores SIGINT", "", 3],
])(
  "stopInvocation: a leader that %s is killed with its TERM-resistant descendants",
  (_, exitOnSigint, escalated) =>
    withFakeTool(async (dir, tool) => {
      const sentinel = Bun.spawn(["sleep", "30"], { detached: true });
      try {
        const env = { ...process.env, FAKE_DEPTH: "2", FAKE_EXIT_ON_SIGINT: exitOnSigint };
        const child = Bun.spawn([tool], {
          env,
          detached: true,
          stdio: ["ignore", "ignore", "ignore"],
        });
        await eventually(() => existsSync(join(dir, "ready")));
        const cleanup = await stopInvocation(child, 200);
        expect(cleanup.tracked_pids).toEqual(pids(dir).toSorted((a, b) => a - b));
        expect(cleanup.escalated_pids).toHaveLength(escalated);
        await eventually(() => survivors(dir, tool).length === 0);
        expect(sentinel.exitCode).toBeNull();
      } finally {
        sentinel.kill();
      }
    }),
  10_000,
);

test.each([
  ["SIGTERM", [], 143, "interrupted", "interrupted by SIGTERM"],
  ["an invocation timeout", ["--run-timeout", "1"], 1, "tool_error", "timed out after 1 seconds"],
])(
  "mutation-run: %s stops the tool and still writes the summary and partial logs",
  (cause, args, exit, status, error) =>
    withFakeTool(async (dir, tool) => {
      const output = join(dir, "report");
      const argv = [process.execPath, join(ROOT, "tools/main.ts"), "mutation-run"];
      const runner = Bun.spawn([...argv, "--root", dir, "--output", output, ...args], {
        env: { ...process.env, CARGO_MUTANTS_BIN: tool, FAKE_EXIT_ON_SIGINT: "1" },
        stdio: ["ignore", "ignore", "ignore"],
      });
      try {
        await eventually(() => existsSync(join(dir, "ready")));
        if (cause === "SIGTERM") runner.kill("SIGTERM");
        expect(await runner.exited).toBe(exit);
        const summary = table(JSON.parse(readFileSync(join(output, "summary.json"), "utf8")));
        expect([summary["status"], String(summary["error"])]).toEqual([
          status,
          expect.stringContaining(error),
        ]);
        expect(readFileSync(join(output, "version.stdout.log"), "utf8")).toBe(
          "partial diagnostic\n",
        );
        expect(survivors(dir, tool)).toEqual([]);
      } finally {
        runner.kill("SIGKILL");
      }
    }),
  15_000,
);
