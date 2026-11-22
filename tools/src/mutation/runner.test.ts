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

test("stopInvocation never adopts an already-reaped leader", async () => {
  const child = Bun.spawn([process.execPath, "-e", ""], { stdio: ["ignore", "ignore", "ignore"] });
  await child.exited;
  expect(await stopInvocation(child, 0)).toEqual({ tracked_pids: [], escalated_pids: [] });
});

/** Exercise the actual CLI with a checkout-local stand-in for process discovery. */
function mutationWithPs(dir: string, tool: string, script: string) {
  writeFileSync(
    join(dir, "ps"),
    `#!${process.execPath}\nimport { writeFileSync } from "node:fs";
writeFileSync(${JSON.stringify(join(dir, "discovery-pid"))}, String(process.pid));
${script}`,
    { mode: 0o755 },
  );
  const output = join(dir, "report");
  const runner = Bun.spawn(
    [
      process.execPath,
      join(ROOT, "tools/main.ts"),
      "mutation-run",
      "--root",
      dir,
      "--output",
      output,
      "--run-timeout",
      "1",
    ],
    {
      env: {
        ...process.env,
        PATH: `${dir}:${process.env.PATH ?? ""}`,
        CARGO_MUTANTS_BIN: tool,
        FAKE_EXIT_ON_SIGINT: "1",
        FAKE_DEPTH: "2",
      },
      stdio: ["ignore", "ignore", "ignore"],
    },
  );
  return { runner, output };
}

test(
  "mutation-run: process discovery tolerates a busy host without abandoning cleanup",
  () =>
    withFakeTool(async (dir, tool) => {
      const ps = Bun.which("ps");
      expect(ps).not.toBeNull();
      const { runner, output } = mutationWithPs(
        dir,
        tool,
        `
await Bun.sleep(1200);
// Fill both pipes before forwarding the real snapshot.
process.stdout.write("\\n".repeat(256 * 1024));
process.stderr.write("diagnostic".repeat(32 * 1024));
const result = Bun.spawnSync([${JSON.stringify(ps)}, ...process.argv.slice(2)], { stdio: ["ignore", "inherit", "inherit"] });
process.exit(result.exitCode);
`,
      );
      try {
        expect(await runner.exited).toBe(1);
        const summary = table(JSON.parse(readFileSync(join(output, "summary.json"), "utf8")));
        expect(summary["error"]).toContain("timed out after 1 seconds");
        expect(summary["cleanup"]).toMatchObject({
          tracked_pids: pids(dir).toSorted((a, b) => a - b),
          escalated_pids: pids(dir)
            .slice(1)
            .toSorted((a, b) => a - b),
        });
        await eventually(() => survivors(dir, tool).length === 0);
      } finally {
        runner.kill("SIGKILL");
      }
    }),
  20_000,
);

test.each([
  ["fails", 'console.error("discovery refused"); process.exit(23);', "discovery refused"],
  ["hangs", "setInterval(() => {}, 1000);", "SIGKILL"],
])(
  "mutation-run: discovery that %s fails closed without signalling the invocation",
  (_, script, message) =>
    withFakeTool(async (dir, tool) => {
      const { runner, output } = mutationWithPs(dir, tool, script);
      const started = performance.now();
      try {
        expect(await runner.exited).toBe(1);
        expect(performance.now() - started).toBeLessThan(10_000);
        const summary = table(JSON.parse(readFileSync(join(output, "summary.json"), "utf8")));
        expect(summary["status"]).toBe("tool_error");
        expect(summary["error"]).toContain(message);
        expect(summary["cleanup"]).toBeUndefined();
        const discoveryPid = readFileSync(join(dir, "discovery-pid"), "utf8");
        const discovery = Bun.spawnSync(["ps", "-p", discoveryPid, "-o", "command="]);
        expect(discovery.stdout.toString()).not.toContain(join(dir, "ps"));
        expect(pids(dir)).toHaveLength(3);
        expect(survivors(dir, tool)).toEqual(pids(dir));
      } finally {
        runner.kill("SIGKILL");
      }
    }),
  15_000,
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
