// Bounded cargo-mutants adapter; all mutations run in cargo-mutants scratch copies.
import {
  closeSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { constants } from "node:os";
import { dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { UsageError } from "../lib/command.ts";
import { ROOT } from "../lib/repo.ts";
import { list, table } from "../lib/value.ts";
import { stopInvocation, type Cleanup } from "./processes.ts";

/** The one cargo-mutants pin; `mutation-install` installs exactly this. */
export const VERSION = "27.0.0";

/** Where `mutation-install` puts the pinned tool, checkout-local. */
export const TOOL_ROOT = join(ROOT, `target/mutation-tools/cargo-mutants-${VERSION}`);

export const HELP = `Install the pinned cargo-mutants ${VERSION} with the repository's Rust
toolchain, under target/mutation-tools (no global overwrite):
  just mutation-rust-install
Alternatively set CARGO_MUTANTS_BIN to that version's cargo-mutants executable.
An exact-version executable on PATH is also accepted; nothing is auto-installed.

With no scope: phux-core, crates/phux-core/src/window.rs. With --file alone or
--in-diff alone: discover across workspace members, test only mutated packages.
--file is a cargo-mutants glob; --package is an exact package name (repeatable).
--in-diff REF uses git merge-base REF HEAD, then git diff BASE against the working
tree (tracked staged/unstaged edits included; untracked files are not in Git diffs).
Fetch the base ref/history in shallow CI first. Files/regex/package/diff intersect.
Test-only/deletion-only diffs may select nothing: this is not coverage evidence.

The deterministic first slice shard selects AT MOST --limit mutants, not a random
sample or full coverage. --list discovers and writes JSON without compiling.
Default: 8 mutants, 1 mutation worker, 2 compiler tasks, build 300s, test 60s,
each tool invocation 1800s. Increase limits explicitly for costly native packages.
Cargo dependencies may resolve across the workspace; only selected packages test.
Scratch copies have private target directories (no shared CARGO_TARGET_DIR), no
target-cache copying, and are removed by cargo-mutants. Registry/git download
caches are reused. This pays a cold package build per run/worker, not all crates.

Artifacts: unique target/mutation/rust-* directory, or NEW --output directory;
summary.json, files.json, candidates.json, selected.json, command logs, and raw
mutants.out/{outcomes.json,mutants.json,log/,diff/}. No score threshold.
Exit 0: completed findings (including survivors, unviable mutants and timeouts),
or listed/empty scope. Exit 1: tool/discovery/incomplete-run error. Exit 4: baseline
failed/timed out. Exit 2: CLI usage error. summary.json records status, counts,
baseline, raw tool exit, selection and commands. Timeouts need investigation;
only 'caught' means killed by tests. Unviable means compile failure, not a kill.
SIGTERM/SIGINT exit 143/130 with status 'interrupted'. Partial stdout/stderr stay
in command logs. Shutdown allows 5s for graceful cleanup, then kills tracked
descendants across process groups, checking PID/start-time identity via ps.

Rust cargo test is not a Rust->FFI->Zig test. For that evidence, each mutated
scratch checkout must rebuild phux-client-ffi (--profile ffi-dev or ffi-release)
and point the Zig host's -Dphux-client-ffi-lib-dir at THAT checkout's archive
directory (and -Dphux-client-ffi-include-dir at its header directory) before
running host tests. cargo-mutants 27 has cargo/nextest test tools, no arbitrary
post-build Zig command hook. This runner does not claim cross-language coverage.
`;

export const USAGE = `[--file GLOB] [--package NAME] [--in-diff REF]
  [--re REGEX] [--features LIST] [--limit N] [--jobs 1-4] [--build-timeout S]
  [--test-timeout S] [--run-timeout S] [--output DIR] [--list] [--root DIR]`;

export interface Options {
  file: string[];
  package: string[];
  inDiff: string | undefined;
  re: string | undefined;
  features: string | undefined;
  limit: number;
  jobs: number;
  buildTimeout: number;
  testTimeout: number;
  runTimeout: number;
  output: string | undefined;
  list: boolean;
  root: string;
}

function positive(name: string, value: string | undefined, fallback: number): number {
  if (value === undefined) return fallback;
  const number = Number(value);
  if (!Number.isInteger(number)) throw new UsageError(`--${name}: invalid int value: '${value}'`);
  if (number < 1) throw new UsageError(`--${name}: must be at least 1`);
  return number;
}

/** Parsed options. A bad command line throws UsageError or a parseArgs error. */
export function parseOptions(args: string[]): Options {
  const { values } = parseArgs({
    args,
    options: {
      file: { type: "string", multiple: true, default: [] },
      package: { type: "string", multiple: true, default: [] },
      "in-diff": { type: "string" },
      re: { type: "string" },
      features: { type: "string" },
      limit: { type: "string" },
      jobs: { type: "string" },
      "build-timeout": { type: "string" },
      "test-timeout": { type: "string" },
      "run-timeout": { type: "string" },
      output: { type: "string" },
      list: { type: "boolean", default: false },
      root: { type: "string" },
    },
  });
  const jobs = positive("jobs", values.jobs, 1);
  if (jobs > 4) throw new UsageError(`--jobs: invalid choice: ${jobs} (choose from 1, 2, 3, 4)`);
  return {
    file: values.file,
    package: values.package,
    inDiff: values["in-diff"],
    re: values.re,
    features: values.features,
    limit: positive("limit", values.limit, 8),
    jobs,
    buildTimeout: positive("build-timeout", values["build-timeout"], 300),
    testTimeout: positive("test-timeout", values["test-timeout"], 60),
    runTimeout: positive("run-timeout", values["run-timeout"], 1800),
    output: values.output,
    list: values.list,
    root: resolve(values.root ?? ROOT),
  };
}

export interface Report {
  schema_version: 1;
  tool_version: string;
  root: string;
  status: string;
  commands: string[][];
  counts: Record<string, number>;
  baseline: unknown[];
  merge_base?: string;
  candidates?: number;
  selected?: number;
  tool_exit?: number;
  cleanup?: Cleanup;
  error?: string;
}

export class Interrupted extends Error {
  constructor(readonly signum: number) {
    super(`interrupted by ${signum === constants.signals.SIGINT ? "SIGINT" : "SIGTERM"}`);
  }
}

class Timeout extends Error {}

/** SIGTERM/SIGINT become an Interrupted the active invocation observes. */
export class Interrupts {
  received: Interrupted | undefined;
  private wake: (() => void) | undefined;
  readonly signalled = new Promise<void>((resolve) => {
    this.wake = resolve;
  });

  install(): void {
    for (const name of ["SIGTERM", "SIGINT"] as const) {
      // Repeated signals must not interrupt cleanup or the final report.
      process.on(name, () => {
        this.received ??= new Interrupted(constants.signals[name]);
        this.wake?.();
      });
    }
  }
}

function writeJson(path: string, data: unknown): void {
  writeFileSync(path, `${JSON.stringify(data, null, 2)}\n`);
}

class Runner {
  constructor(
    private readonly options: Options,
    private readonly output: string,
    private readonly report: Report,
    private readonly interrupts: Interrupts,
  ) {}

  /** File-backed output survives interruption and cannot block pipe draining. */
  async invoke(command: string[], label: string): Promise<[number, string]> {
    if (this.interrupts.received) throw this.interrupts.received;
    this.report.commands.push(command);
    const env = { ...process.env };
    delete env["CARGO_TARGET_DIR"];
    delete env["CARGO_BUILD_TARGET_DIR"];
    const stdoutPath = join(this.output, `${label}.stdout.log`);
    const stdout = openSync(stdoutPath, "w");
    const stderr = openSync(join(this.output, `${label}.stderr.log`), "w");
    try {
      const child = Bun.spawn(command, {
        cwd: this.options.root,
        env,
        stdin: "ignore",
        stdout,
        stderr,
        detached: true,
      });
      let timer: ReturnType<typeof setTimeout> | undefined;
      const expired = new Promise<"timeout">((done) => {
        timer = setTimeout(() => done("timeout"), this.options.runTimeout * 1000);
      });
      const outcome = await Promise.race([
        child.exited,
        expired,
        this.interrupts.signalled.then(() => "interrupted" as const),
      ]);
      clearTimeout(timer);
      if (typeof outcome !== "number") {
        this.report.cleanup = await stopInvocation(child);
        if (outcome === "interrupted" && this.interrupts.received) throw this.interrupts.received;
        throw new Timeout(
          `Command '${JSON.stringify(command)}' timed out after ${this.options.runTimeout} seconds`,
        );
      }
      return [outcome, readFileSync(stdoutPath, "utf8")];
    } finally {
      closeSync(stdout);
      closeSync(stderr);
    }
  }

  async checked(command: string[], label: string): Promise<string> {
    const [code, stdout] = await this.invoke(command, label);
    if (code) throw new Error(`${label} exited ${code}; see ${this.output}/${label}.stderr.log`);
    return stdout;
  }

  async tool(): Promise<string[]> {
    const local = join(TOOL_ROOT, "bin/cargo-mutants");
    let binary = process.env["CARGO_MUTANTS_BIN"] || Bun.which("cargo-mutants");
    if (existsSync(local)) binary = process.env["CARGO_MUTANTS_BIN"] ?? local;
    if (!binary) throw new Error("cargo-mutants missing; run just mutation-rust-install");
    const command = [binary, "mutants"];
    const version = (await this.checked([...command, "--version"], "version")).trim();
    if (version !== `cargo-mutants ${VERSION}`) {
      throw new Error(`expected cargo-mutants ${VERSION}; found ${version}`);
    }
    return command;
  }

  async scope(): Promise<string[]> {
    const options = this.options;
    const flags: string[] = [];
    if (options.file.length === 0 && options.package.length === 0 && !options.inDiff) {
      options.package = ["phux-core"];
      options.file = ["crates/phux-core/src/window.rs"];
    }
    if (options.package.length === 0) flags.push("--workspace");
    for (const name of options.package) flags.push("--package", name);
    for (const file of options.file) flags.push("--file", file);
    if (options.re) flags.push("--re", options.re);
    if (options.features) flags.push("--features", options.features);
    if (options.inDiff) flags.push(...(await this.diffScope(options.inDiff)));
    return flags;
  }

  async diffScope(ref: string): Promise<string[]> {
    const base = (await this.checked(["git", "merge-base", ref, "HEAD"], "merge-base")).trim();
    this.report.merge_base = base;
    const diff = await this.checked(
      [
        "git",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--unified=0",
        base,
        "--",
      ],
      "git-diff",
    );
    const path = join(this.output, "scope.diff");
    writeFileSync(path, diff);
    return ["--in-diff", path];
  }

  async listJson(command: string[], label: string): Promise<unknown[]> {
    // cargo-mutants emits no JSON for an empty diff.
    const raw = await this.checked([...command, "--list", "--json"], label);
    return list(JSON.parse(raw.trim() || "[]"));
  }

  async discover(command: string[]): Promise<string[]> {
    const files = await this.checked([...command, "--list-files", "--json"], "files");
    writeJson(join(this.output, "files.json"), JSON.parse(files));
    const candidates = await this.listJson(command, "candidates");
    writeJson(join(this.output, "candidates.json"), candidates);
    this.report.candidates = candidates.length;
    const shards = Math.max(1, Math.ceil(candidates.length / this.options.limit));
    const sharded = [...command, "--shard", `0/${shards}`, "--sharding", "slice"];
    const selected = await this.listJson(sharded, "selected");
    writeJson(join(this.output, "selected.json"), selected);
    if (selected.length > this.options.limit) {
      throw new Error("cargo-mutants shard exceeded requested limit");
    }
    this.report.selected = selected.length;
    return sharded;
  }

  outcomes(code: number): number {
    const report = this.report;
    report.tool_exit = code;
    const raw = table(
      JSON.parse(readFileSync(join(this.output, "mutants.out/outcomes.json"), "utf8")),
    );
    const counts: Record<string, number> = {};
    for (const key of ["caught", "missed", "unviable", "timeout"]) {
      const count = raw[key];
      if (typeof count !== "number") throw new Error(`outcomes.json: ${key} is not a count`);
      counts[key] = count;
    }
    report.counts = counts;
    report.baseline = list(raw["outcomes"]).filter((row) => table(row)["scenario"] === "Baseline");
    if (code === 4) {
      report.status = "baseline_failed";
      return 4;
    }
    if (![0, 2, 3].includes(code)) throw new Error(`cargo-mutants exited ${code}`);
    const total = Object.values(counts).reduce((sum, count) => sum + count, 0);
    if (total !== report.selected) throw new Error("incomplete or unclassified mutation outcomes");
    if (table(report.baseline[0])["summary"] !== "Success") {
      throw new Error("missing successful baseline");
    }
    report.status = "completed";
    return 0;
  }

  async run(): Promise<number> {
    const options = this.options;
    let command = [
      ...(await this.tool()),
      "--no-config",
      "--no-shuffle",
      "--gitignore=true",
      "--test-workspace=false",
      "--test-tool=cargo",
      "--baseline=run",
      "--colors=never",
      "--annotations=none",
      "--cap-lints=true",
      "--cargo-arg=--locked",
      "--cargo-arg=--target-dir=target",
      "--jobs",
      String(options.jobs),
      "--jobserver-tasks",
      String(options.jobs * 2),
      "--build-timeout",
      String(options.buildTimeout),
      "--timeout",
      String(options.testTimeout),
    ];
    command.push(...(await this.scope()));
    command = await this.discover(command);
    if (this.report.selected === 0) {
      this.report.status = "empty_scope";
      return 0;
    }
    if (options.list) {
      this.report.status = "listed";
      return 0;
    }
    command.push("--output", this.output, "--caught", "--unviable");
    const [code, stdout] = await this.invoke(command, "run");
    process.stdout.write(stdout);
    return this.outcomes(code);
  }
}

/** A NEW --output directory, or a unique one under target/mutation. */
function outputDirectory(options: Options): string {
  if (options.output !== undefined) {
    const output = resolve(options.output);
    mkdirSync(dirname(output), { recursive: true });
    mkdirSync(output); // Never reuses an existing report directory.
    return output;
  }
  const parent = join(options.root, "target/mutation");
  mkdirSync(parent, { recursive: true });
  return mkdtempSync(join(parent, "rust-"));
}

export async function mutationRun(options: Options, interrupts: Interrupts): Promise<number> {
  const output = outputDirectory(options);
  const report: Report = {
    schema_version: 1,
    tool_version: VERSION,
    root: options.root,
    status: "tool_error",
    commands: [],
    counts: {},
    baseline: [],
  };
  let code = 1;
  try {
    code = await new Runner(options, output, report, interrupts).run();
    if (interrupts.received) throw interrupts.received;
  } catch (error) {
    if (error instanceof Interrupted) {
      report.status = "interrupted";
      report.error = error.message;
      code = 128 + error.signum;
    } else {
      report.status = "tool_error";
      report.error = error instanceof Error ? error.message : String(error);
      console.error(`error: ${report.error}`);
    }
  } finally {
    writeJson(join(output, "summary.json"), report);
    console.log(`Rust mutation report: ${output}/summary.json`);
  }
  return code;
}
