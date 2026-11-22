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
import { describe, Failure, UsageError } from "../lib/command.ts";
import { ROOT } from "../lib/repo.ts";
import { list, table } from "../lib/value.ts";
import { stopInvocation, type Cleanup } from "./processes.ts";

/** The one cargo-mutants pin; `mutation-install` installs exactly this. */
export const VERSION = "27.0.0";

/** Where `mutation-install` puts the pinned tool, checkout-local. */
export const TOOL_ROOT = join(ROOT, `target/mutation-tools/cargo-mutants-${VERSION}`);

export const USAGE = `[--file GLOB] [--package NAME] [--in-diff REF] [--re REGEX]
  [--limit N] [--jobs 1-4] [--build-timeout S] [--test-timeout S] [--run-timeout S]
  [--output DIR] [--list] [--root DIR]`;

export const HELP = `Needs cargo-mutants ${VERSION}: just mutation-rust-install, CARGO_MUTANTS_BIN, or PATH.
Default scope: phux-core's crates/phux-core/src/window.rs. Scope filters intersect;
--in-diff REF diffs the merge base of REF and HEAD against tracked working-tree text.
Selects at most --limit mutants (default 8), the first deterministic slice.
--list writes the selection without compiling. Defaults: --jobs 1, --build-timeout 300,
--test-timeout 60, --run-timeout 1800 (seconds, per tool invocation).
Reports: a new target/mutation/rust-* directory or a NEW --output directory.
Exit 0 findings or empty scope, 1 tool error, 2 usage, 4 baseline failed, 130/143 signal.
See docs/development.md#mutation-testing.`;

const BOUNDS = { limit: 8, jobs: 1, "build-timeout": 300, "test-timeout": 60, "run-timeout": 1800 };

/** Parsed options. A bad command line throws UsageError or a parseArgs error. */
export function parseOptions(args: string[]) {
  const text = { type: "string" } as const;
  const { values } = parseArgs({
    args,
    options: {
      file: { type: "string", multiple: true, default: [] },
      package: { type: "string", multiple: true, default: [] },
      "in-diff": text,
      re: text,
      limit: text,
      jobs: text,
      "build-timeout": text,
      "test-timeout": text,
      "run-timeout": text,
      output: text,
      list: { type: "boolean", default: false },
      root: text,
    },
  });
  const bound = (name: keyof typeof BOUNDS) => {
    const value = values[name];
    const number = Number(value ?? BOUNDS[name]);
    if (!Number.isInteger(number)) throw new UsageError(`--${name}: invalid int value: '${value}'`);
    if (number < 1) throw new UsageError(`--${name}: must be at least 1`);
    return number;
  };
  const jobs = bound("jobs");
  if (jobs > 4) throw new UsageError(`--jobs: invalid choice: ${jobs} (choose from 1, 2, 3, 4)`);
  return {
    ...values,
    root: resolve(values.root ?? ROOT),
    limit: bound("limit"),
    jobs,
    buildTimeout: bound("build-timeout"),
    testTimeout: bound("test-timeout"),
    runTimeout: bound("run-timeout"),
  };
}

export type Options = ReturnType<typeof parseOptions>;

interface Report {
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

type Signal = "SIGINT" | "SIGTERM";

class Interrupted extends Error {
  constructor(readonly signal: Signal) {
    super(`interrupted by ${signal}`);
  }
}

/** The first SIGTERM/SIGINT; later ones must not interrupt cleanup or the report. */
class Interrupts {
  received: Interrupted | undefined;
  readonly signalled = new Promise<"interrupted">((wake) => {
    for (const signal of ["SIGTERM", "SIGINT"] as const) {
      process.on(signal, () => {
        this.received ??= new Interrupted(signal);
        wake("interrupted");
      });
    }
  });

  check(): void {
    if (this.received) throw this.received;
  }
}

function writeJson(path: string, data: unknown): void {
  writeFileSync(path, `${JSON.stringify(data, null, 2)}\n`);
}

const BASE_FLAGS = [
  "--no-config --no-shuffle --gitignore=true --test-workspace=false --test-tool=cargo",
  "--baseline=run --colors=never --annotations=none --cap-lints=true",
  "--cargo-arg=--locked --cargo-arg=--target-dir=target",
]
  .join(" ")
  .split(" ");

const DIFF = "diff --no-ext-diff --no-textconv --no-renames --src-prefix=a/ --dst-prefix=b/";

class Runner {
  constructor(
    private readonly options: Options,
    private readonly output: string,
    private readonly report: Report,
    private readonly interrupts: Interrupts,
  ) {}

  /** File-backed output survives interruption and cannot block pipe draining. */
  async invoke(command: string[], label: string): Promise<[number, string]> {
    this.interrupts.check();
    this.report.commands.push(command);
    const env = { ...process.env, CARGO_TARGET_DIR: undefined, CARGO_BUILD_TARGET_DIR: undefined };
    const stdoutPath = join(this.output, `${label}.stdout.log`);
    const stdout = openSync(stdoutPath, "w");
    const stderr = openSync(join(this.output, `${label}.stderr.log`), "w");
    let timer: Timer | undefined;
    try {
      const child = Bun.spawn(command, {
        cwd: this.options.root,
        env,
        stdio: ["ignore", stdout, stderr],
        detached: true,
      });
      const expired = new Promise<"timeout">((done) => {
        timer = setTimeout(() => done("timeout"), this.options.runTimeout * 1000);
      });
      const outcome = await Promise.race([child.exited, expired, this.interrupts.signalled]);
      if (typeof outcome === "number") return [outcome, readFileSync(stdoutPath, "utf8")];
      this.report.cleanup = await stopInvocation(child);
      this.interrupts.check();
      throw new Failure(
        `Command '${JSON.stringify(command)}' timed out after ${this.options.runTimeout} seconds`,
      );
    } finally {
      clearTimeout(timer);
      closeSync(stdout);
      closeSync(stderr);
    }
  }

  async checked(command: string[], label: string): Promise<string> {
    const [code, stdout] = await this.invoke(command, label);
    if (code) throw new Failure(`${label} exited ${code}; see ${this.output}/${label}.stderr.log`);
    return stdout;
  }

  async tool(): Promise<string[]> {
    const local = join(TOOL_ROOT, "bin/cargo-mutants");
    const binary =
      process.env["CARGO_MUTANTS_BIN"] || (existsSync(local) ? local : Bun.which("cargo-mutants"));
    if (!binary) throw new Failure("cargo-mutants missing; run just mutation-rust-install");
    const command = [binary, "mutants"];
    const version = (await this.checked([...command, "--version"], "version")).trim();
    if (version !== `cargo-mutants ${VERSION}`) {
      throw new Failure(`expected cargo-mutants ${VERSION}; found ${version}`);
    }
    return command;
  }

  async scope(): Promise<string[]> {
    const { "in-diff": inDiff, re } = this.options;
    const unscoped = !inDiff && this.options.file.length + this.options.package.length === 0;
    const file = unscoped ? ["crates/phux-core/src/window.rs"] : this.options.file;
    const packages = unscoped ? ["phux-core"] : this.options.package;
    const flags = packages.length === 0 ? ["--workspace"] : [];
    for (const name of packages) flags.push("--package", name);
    for (const glob of file) flags.push("--file", glob);
    if (re) flags.push("--re", re);
    if (inDiff) flags.push("--in-diff", await this.diff(inDiff));
    return flags;
  }

  /** The merge-base diff file cargo-mutants reads; tracked uncommitted edits included. */
  async diff(ref: string): Promise<string> {
    const base = (await this.checked(["git", "merge-base", ref, "HEAD"], "merge-base")).trim();
    this.report.merge_base = base;
    const argv = ["git", ...DIFF.split(" "), "--unified=0", base, "--"];
    const path = join(this.output, "scope.diff");
    writeFileSync(path, await this.checked(argv, "git-diff"));
    return path;
  }

  async listJson(command: string[], label: string): Promise<unknown[]> {
    // cargo-mutants emits no JSON for an empty diff.
    const raw = await this.checked([...command, "--list", "--json"], label);
    const rows = list(JSON.parse(raw.trim() || "[]"));
    writeJson(join(this.output, `${label}.json`), rows);
    return rows;
  }

  async discover(command: string[]): Promise<string[]> {
    const files = await this.checked([...command, "--list-files", "--json"], "files");
    writeJson(join(this.output, "files.json"), JSON.parse(files));
    const candidates = await this.listJson(command, "candidates");
    this.report.candidates = candidates.length;
    const shards = Math.max(1, Math.ceil(candidates.length / this.options.limit));
    const sharded = [...command, "--shard", `0/${shards}`, "--sharding", "slice"];
    const selected = await this.listJson(sharded, "selected");
    if (selected.length > this.options.limit) {
      throw new Failure("cargo-mutants shard exceeded requested limit");
    }
    this.report.selected = selected.length;
    return sharded;
  }

  outcomes(code: number): number {
    const report = this.report;
    report.tool_exit = code;
    const outcomes = join(this.output, "mutants.out/outcomes.json");
    const raw = table(JSON.parse(readFileSync(outcomes, "utf8")));
    for (const key of ["caught", "missed", "unviable", "timeout"]) {
      const count = raw[key];
      if (typeof count !== "number") throw new Failure(`outcomes.json: ${key} is not a count`);
      report.counts[key] = count;
    }
    report.baseline = list(raw["outcomes"]).filter((row) => table(row)["scenario"] === "Baseline");
    if (code === 4) {
      report.status = "baseline_failed";
      return 4;
    }
    if (![0, 2, 3].includes(code)) throw new Failure(`cargo-mutants exited ${code}`);
    const total = Object.values(report.counts).reduce((sum, count) => sum + count, 0);
    if (total !== report.selected)
      throw new Failure("incomplete or unclassified mutation outcomes");
    if (table(report.baseline[0])["summary"] !== "Success") {
      throw new Failure("missing successful baseline");
    }
    report.status = "completed";
    return 0;
  }

  async run(): Promise<number> {
    const { jobs, buildTimeout, testTimeout } = this.options;
    const budget = [`--jobs=${jobs}`, `--jobserver-tasks=${jobs * 2}`];
    budget.push(`--build-timeout=${buildTimeout}`, `--timeout=${testTimeout}`);
    const tool = [...(await this.tool()), ...BASE_FLAGS, ...budget];
    const command = await this.discover([...tool, ...(await this.scope())]);
    if (this.report.selected === 0) {
      this.report.status = "empty_scope";
      return 0;
    }
    if (this.options.list) {
      this.report.status = "listed";
      return 0;
    }
    const [code, stdout] = await this.invoke(
      [...command, "--output", this.output, "--caught", "--unviable"],
      "run",
    );
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

/** One bounded run; summary.json is written whatever happens. */
export async function mutationRun(options: Options): Promise<number> {
  const interrupts = new Interrupts();
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
  try {
    const code = await new Runner(options, output, report, interrupts).run();
    interrupts.check();
    return code;
  } catch (error) {
    report.error = describe(error);
    if (!(error instanceof Interrupted)) throw new Failure(report.error);
    report.status = "interrupted";
    return 128 + constants.signals[error.signal];
  } finally {
    writeJson(join(output, "summary.json"), report);
    console.log(`Rust mutation report: ${output}/summary.json`);
  }
}
