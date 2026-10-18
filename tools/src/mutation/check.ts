// Real-tool acceptance checks using disposable Rust fixtures, not production edits.
//
// Requires the exact runner pin (CARGO_MUTANTS_BIN or the checkout-local
// install), Git, and the repository Rust toolchain. Reports remain under
// target/mutation/rust-check-*/; fixtures are removed.
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { deepEqual, list, str, table, type Table } from "../lib/value.ts";

const RUNNER = [process.execPath, join(ROOT, "tools/main.ts"), "mutation-run"];

const SOURCE = `pub fn increment(n: u32) -> u32 { n + 1 }
pub fn even(n: u32) -> bool { n % 2 == 0 }
pub struct Token;
pub fn token() -> Token { Token }
pub fn ready() -> bool { true }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_increment_contract() {
        // Deliberately incomplete: constant 1 satisfies this one input.
        assert_eq!(increment(0), 1);
    }
    #[test]
    fn parity_contract() {
        assert!(even(4));
        assert!(!even(3));
    }
    #[test]
    fn readiness_wait() {
        if !ready() { std::thread::sleep(std::time::Duration::from_secs(5)); }
    }
}
`;

// A stand-in cargo-mutants whose `--version` ignores SIGTERM/SIGINT and
// leaves a chain of descendants, each in a session of its own.
const TOPOLOGY = `#!${process.execPath}
import { writeFileSync, writeSync } from "node:fs";
import { basename, join } from "node:path";

const directory = import.meta.dir;
const role = process.argv.at(-1) ?? "";
// Exit early during graceful cleanup so descendants are reparented.
const exitOnInterrupt = role === "--version" && basename(directory) === "external-sigterm";
process.on("SIGTERM", () => {});
process.on("SIGINT", () => {
  if (exitOnInterrupt) process.exit(0);
});
writeFileSync(join(directory, role + ".pid"), String(process.pid));
writeSync(1, "partial diagnostic: " + role + "\\n");
const next = role === "--version" ? "middle" : role === "middle" ? "leaf" : undefined;
if (next === undefined) {
  writeFileSync(join(directory, "ready"), "");
} else {
  Bun.spawn([process.execPath, import.meta.path, next], {
    detached: true,
    stdio: ["ignore", "inherit", "inherit"],
  }).unref();
}
setInterval(() => {}, 1000);
`;

export class CheckFailed extends Error {}

function check(condition: boolean, detail: unknown): void {
  if (!condition) throw new CheckFailed(JSON.stringify(detail, null, 2));
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** stdout+stderr of a command that must succeed. */
function command(argv: string[], cwd: string): string {
  const result = Bun.spawnSync(argv, { cwd, stdout: "pipe", stderr: "pipe" });
  const output = result.stdout.toString() + result.stderr.toString();
  if (result.exitCode !== 0)
    throw new CheckFailed(`${argv.join(" ")} exited ${result.exitCode}: ${output}`);
  return result.stdout.toString();
}

function commit(root: string, message: string): void {
  command(
    [
      "git",
      "-c",
      "user.name=Mutation fixture",
      "-c",
      "user.email=fixture@example.invalid",
      "-c",
      "core.hooksPath=/dev/null",
      "commit",
      "-qm",
      message,
    ],
    root,
  );
}

function fixture(root: string): void {
  mkdirSync(join(root, "src"));
  writeFileSync(join(root, "src/lib.rs"), SOURCE);
  writeFileSync(
    join(root, "Cargo.toml"),
    '[package]\nname = "rust-mutation-fixture"\nversion = "0.0.0"\nedition = "2024"\n',
  );
  writeFileSync(join(root, ".gitignore"), "/target/\n");
  copyFileSync(join(ROOT, "rust-toolchain.toml"), join(root, "rust-toolchain.toml"));
  command(["cargo", "generate-lockfile"], root);
  command(["git", "init", "-q"], root);
  command(["git", "add", "."], root);
  commit(root, "base fixture");
}

function readJson(path: string): Table {
  return table(JSON.parse(readFileSync(path, "utf8")));
}

/** One runner invocation that must leave the fixture checkout untouched. */
function run(root: string, reports: string, name: string, options: string[], expected = 0): Table {
  const output = join(reports, name);
  const before = command(["git", "status", "--porcelain"], root);
  const source = readFileSync(join(root, "src/lib.rs"));
  const result = Bun.spawnSync(
    [
      ...RUNNER,
      "--root",
      root,
      "--output",
      output,
      "--package",
      "rust-mutation-fixture",
      ...options,
    ],
    { stdio: ["ignore", "inherit", "inherit"] },
  );
  check(result.exitCode === expected, [name, result.exitCode]);
  check(command(["git", "status", "--porcelain"], root) === before, name);
  check(readFileSync(join(root, "src/lib.rs")).equals(source), name);
  return readJson(join(output, "summary.json"));
}

function rows(value: unknown): Table[] {
  return list(value).map(table);
}

function classifications(root: string, reports: string): void {
  const report = run(root, reports, "classifications", [
    "--re",
    "replace increment -> u32 with 1$|replace even -> bool with true$|" +
      "replace token -> Token with Default::default|replace ready -> bool with false$",
    "--test-timeout",
    "1",
  ]);
  check(report["status"] === "completed", report);
  check(deepEqual(report["counts"], { caught: 1, missed: 1, unviable: 1, timeout: 1 }), report);
  const raw = readJson(join(reports, "classifications/mutants.out/outcomes.json"));
  const bySummary = new Map(rows(raw["outcomes"]).map((row) => [str(row["summary"]), row]));
  const lastPhase = (summary: string) =>
    table(rows(bySummary.get(summary)?.["phase_results"]).at(-1))["phase"];
  check(lastPhase("CaughtMutant") === "Test", raw);
  check(lastPhase("Unviable") === "Build", raw);
  check(bySummary.get("Success")?.["scenario"] === "Baseline", raw);
}

function diffAndSample(root: string, reports: string): void {
  const base = command(["git", "rev-parse", "HEAD"], root).trim();
  const source = join(root, "src/lib.rs");
  writeFileSync(source, `${SOURCE}\npub fn added(n: u32) -> u32 { n * 2 }\n`);
  command(["git", "add", "src/lib.rs"], root);
  commit(root, "add function");
  const options = ["--in-diff", base, "--list", "--limit", "2"];
  let report = run(root, reports, "diff-list", options);
  check(report["merge_base"] === base, report);
  check(report["status"] === "listed", report);
  const selectedCount = Number(report["selected"]);
  check(0 < selectedCount && selectedCount <= 2 && 2 < Number(report["candidates"]), report);
  const selected = rows(JSON.parse(readFileSync(join(reports, "diff-list/selected.json"), "utf8")));
  check(
    selected.every((row) => table(row["function"])["function_name"] === "added"),
    selected,
  );
  run(root, reports, "diff-repeat", options);
  check(
    readFileSync(join(reports, "diff-list/selected.json")).equals(
      readFileSync(join(reports, "diff-repeat/selected.json")),
    ),
    "repeated selection differs",
  );
  report = run(root, reports, "empty-diff", ["--in-diff", "HEAD"]);
  check(report["status"] === "empty_scope", report);
  check(report["selected"] === 0, report);
  // Tracked, uncommitted edits must be compared against the real new-side text.
  writeFileSync(source, readFileSync(source, "utf8").replace("n * 2", "n * 3"));
  report = run(root, reports, "dirty-diff", ["--in-diff", "HEAD", "--list", "--limit", "1"]);
  check(report["selected"] === 1, report);
}

function baselineFailure(root: string, reports: string): void {
  writeFileSync(
    join(root, "src/lib.rs"),
    SOURCE.replace("assert_eq!(increment(0), 1)", "assert_eq!(increment(0), 9)"),
  );
  const report = run(root, reports, "baseline-failed", ["--limit", "1"], 4);
  check(report["status"] === "baseline_failed", report);
  const counts = Object.values(table(report["counts"])).map(Number);
  check(counts.reduce((sum, count) => sum + count, 0) === 0, report);
  check(table(list(report["baseline"])[0])["summary"] === "Failure", report);
}

function invalidRef(root: string, reports: string): void {
  const report = run(root, reports, "invalid-ref", ["--in-diff", "refs/heads/absent-base"], 1);
  check(report["status"] === "tool_error", report);
  check(String(report["error"]).includes("merge-base exited"), report);
  check(deepEqual(report["baseline"], []), report);
}

function invocationTimeout(root: string, reports: string): void {
  writeFileSync(join(root, "src/lib.rs"), SOURCE.replace("if !ready()", "if ready()"));
  const report = run(
    root,
    reports,
    "invocation-timeout",
    ["--run-timeout", "1", "--limit", "1"],
    1,
  );
  check(report["status"] === "tool_error", report);
  check(String(report["error"]).includes("timed out"), report);
  // cargo-mutants starts Cargo/tests in separate process groups: interrupting
  // just its group must still let its handler reap the fixture test process.
  check(!command(["ps", "-axo", "command="], root).includes(basename(root)), "fixture survived");
}

function topologyPids(directory: string): number[] {
  return readdirSync(directory)
    .filter((name) => name.endsWith(".pid"))
    .map((name) => Number(readFileSync(join(directory, name), "utf8")));
}

function topologyAlive(pid: number, helper: string): boolean {
  const result = Bun.spawnSync(["ps", "-p", String(pid), "-o", "command="], { stdout: "pipe" });
  return result.stdout.toString().includes(helper);
}

async function waitReady(directory: string): Promise<void> {
  const deadline = performance.now() + 5000;
  while (!existsSync(join(directory, "ready"))) {
    if (performance.now() > deadline) throw new CheckFailed("topology did not start");
    await sleep(20);
  }
}

interface Evidence {
  exit: number | null;
  summary: Table | null;
  partial_stdout: string;
  survivors: number[];
  elapsed?: number;
  stdout?: string;
  stderr?: string;
  unrelated_survived?: boolean;
}

async function checkTopology(root: string, reports: string, mode: string): Promise<Evidence> {
  const directory = join(root, "target", mode);
  mkdirSync(directory, { recursive: true });
  const helper = join(directory, "topology.ts");
  writeFileSync(helper, TOPOLOGY);
  chmodSync(helper, 0o755);
  const output = join(reports, mode);
  const source = readFileSync(join(root, "src/lib.rs"));
  const before = command(["git", "status", "--porcelain"], root);
  const started = performance.now();
  const sentinel = Bun.spawn(["sleep", "30"], {
    detached: true,
    stdio: ["ignore", "ignore", "ignore"],
  });
  const runner = Bun.spawn([...RUNNER, "--root", root, "--output", output, "--run-timeout", "2"], {
    env: { ...process.env, CARGO_MUTANTS_BIN: helper },
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  let evidence: Evidence;
  try {
    await waitReady(directory);
    if (mode === "external-sigterm") runner.kill("SIGTERM");
    const finished = await Promise.race([
      runner.exited.then(() => true),
      sleep(10_000).then(() => false),
    ]);
    const [stdout, stderr] = finished
      ? [await new Response(runner.stdout).text(), await new Response(runner.stderr).text()]
      : ["", "runner did not terminate within 10 seconds"];
    const summary = join(output, "summary.json");
    const log = join(output, "version.stdout.log");
    evidence = {
      exit: runner.exitCode,
      summary: existsSync(summary) ? readJson(summary) : null,
      partial_stdout: existsSync(log) ? readFileSync(log, "utf8") : "",
      survivors: topologyPids(directory).filter((pid) => topologyAlive(pid, helper)),
      elapsed: (performance.now() - started) / 1000,
      stdout,
      stderr,
      unrelated_survived: sentinel.exitCode === null && sentinel.signalCode === null,
    };
    writeFileSync(join(reports, `${mode}-evidence.json`), JSON.stringify(evidence, null, 2));
  } finally {
    sentinel.kill();
    if (runner.exitCode === null && runner.signalCode === null) runner.kill("SIGKILL");
    // Red tests must clean up the exact helper processes they created.
    for (const pid of topologyPids(directory)) {
      if (topologyAlive(pid, helper)) process.kill(pid, "SIGKILL");
    }
  }
  check(readFileSync(join(root, "src/lib.rs")).equals(source), "fixture source changed");
  check(command(["git", "status", "--porcelain"], root) === before, "fixture status changed");
  return evidence;
}

function checkTopologyEvidence(result: Evidence): void {
  check(result.survivors.length === 0, result);
  check((result.exit ?? 0) > 0, result);
  check(["interrupted", "tool_error"].includes(String(result.summary?.["status"])), result);
  check(result.partial_stdout.includes("partial diagnostic:"), result);
  check((result.elapsed ?? Infinity) < 10, result);
  check(result.unrelated_survived === true, result);
}

async function processChecks(root: string, reports: string): Promise<void> {
  const results = [
    await checkTopology(root, reports, "external-sigterm"),
    await checkTopology(root, reports, "detached-timeout"),
  ];
  console.log("Process topology evidence:", reports);
  for (const result of results) checkTopologyEvidence(result);
  const [sigterm, timeout] = results;
  check(sigterm?.exit === 143, sigterm);
  check(sigterm?.summary?.["status"] === "interrupted", sigterm);
  check(timeout?.exit === 1, timeout);
}

/** Every acceptance check, or only the process-topology ones. */
export async function mutationCheck(processOnly: boolean): Promise<string> {
  const parent = join(ROOT, "target/mutation");
  mkdirSync(parent, { recursive: true });
  const reports = mkdtempSync(join(parent, "rust-check-"));
  // Respect the harness's approved scratch root on macOS; portable elsewhere.
  const scratch = existsSync("/private/tmp/opencode") ? "/private/tmp/opencode" : tmpdir();
  const root = mkdtempSync(join(scratch, "rust-fixture-"));
  try {
    fixture(root);
    await processChecks(root, reports);
    if (processOnly) return reports;
    classifications(root, reports);
    diffAndSample(root, reports);
    invalidRef(root, reports);
    invocationTimeout(root, reports);
    baselineFailure(root, reports);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  return reports;
}
