import { PhuxError } from "./errors.js";
import {
  DEFAULT_MAX_OUTPUT_BYTES,
  nodeProcessRunner,
  type ProcessResult,
  type ProcessRunner,
  type RunRequest,
} from "./runner.js";
import {
  parseAgentRecord,
  parseAgentStateList,
  parseAskedEvent,
  parseCreateResult,
  parseInsertPaneResult,
  parseLaunchResult,
  parseMovePaneResult,
  parseRenderedFrame,
  parseRunResult,
  parseScreenState,
  parseSessionList,
  parseSpawnResult,
  parseSwapPaneResult,
  parseWatchEvent,
  SchemaValidationError,
  type AgentRecord,
  type AgentStateList,
  type AskedEvent,
  type CreateResult,
  type InsertPaneResult,
  type LaunchResult,
  type MovePaneResult,
  type RenderedFrame,
  type RunResult,
  type ScreenState,
  type SessionList,
  type SpawnResult,
  type SwapPaneResult,
  type TagRow,
  type WatchEvent,
} from "./schemas.js";

export interface PhuxCliOptions {
  readonly executable?: string;
  readonly socket?: string;
  readonly cwd?: string;
  readonly env?: NodeJS.ProcessEnv;
  readonly runner?: ProcessRunner;
  readonly maxStdoutBytes?: number;
  readonly maxStderrBytes?: number;
}

export interface ExecutionOptions {
  /** Abort this local subprocess invocation. */
  readonly signal?: AbortSignal;
  /** Kill this local subprocess if it has not exited within this many ms. */
  readonly timeoutMs?: number;
}

export interface SnapshotOptions extends ExecutionOptions {
  readonly target?: string;
  /** true or zero means all retained history; a positive number bounds it. */
  readonly scrollback?: boolean | number;
  readonly cells?: boolean;
}

export interface WaitOptions extends ExecutionOptions {
  readonly target?: string;
  readonly until?: string;
  readonly idleMs?: number;
  /** phux's own wait deadline, in seconds (distinct from local timeoutMs). */
  readonly phuxTimeoutSeconds?: number;
}

export type WaitOutcome =
  | { readonly outcome: "satisfied"; readonly screen: ScreenState }
  | { readonly outcome: "timed_out"; readonly screen: ScreenState };

export interface CreateOptions extends ExecutionOptions {
  readonly cwd?: string;
  readonly command?: readonly string[];
}

export interface RunOptions extends ExecutionOptions {
  /** phux's own sentinel deadline, in seconds (distinct from local timeoutMs). */
  readonly phuxTimeoutSeconds?: number;
}

export interface AgentTargetOptions extends ExecutionOptions {
  readonly target: string;
}

export type SplitDirection = "horizontal" | "vertical";

export interface PlacementOptions {
  readonly target?: string;
  readonly split?: SplitDirection;
  readonly ratio?: number;
}

export interface SpawnOptions extends ExecutionOptions, PlacementOptions {
  readonly satellite?: string;
  readonly cwd?: string;
  readonly command?: readonly string[];
}

export interface LaunchOptions extends ExecutionOptions, PlacementOptions {
  readonly cwd?: string;
  readonly extra?: readonly string[];
}

export interface SpatialOptions extends ExecutionOptions {
  readonly direction?: SplitDirection;
  readonly ratio?: number;
}

export interface RenderedSnapshotOptions extends ExecutionOptions {
  readonly session?: string;
  readonly cols: number;
  readonly rows: number;
}

export interface AskOptions extends ExecutionOptions {
  readonly id?: string;
  readonly suggestions?: readonly string[];
  readonly elapsedSeconds?: number;
}

export type TerminalSignal = "interrupt" | "freeze" | "resume" | "terminate" | "kill";
export type TagAction = "ls" | "add" | "rm";

export interface WatchOptions extends ExecutionOptions {
  readonly target: string;
  /** Required collection window. The streaming CLI is always stopped after this bound. */
  readonly durationMs: number;
  readonly maxEvents: number;
}

export interface WatchCollection {
  readonly events: readonly WatchEvent[];
  readonly truncated: boolean;
  readonly ended: boolean;
}

export interface PhuxProbe {
  readonly available: boolean;
  readonly version?: string;
  readonly rawVersion?: string;
  readonly reason?: string;
}

export const MINIMUM_PHUX_VERSION = "0.1.0";

const VERSION_PATTERN = /^phux\s+v?(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$/;

export class PhuxCli {
  readonly executable: string;
  readonly socket: string | undefined;
  private readonly cwd: string | undefined;
  private readonly env: NodeJS.ProcessEnv | undefined;
  private readonly runner: ProcessRunner;
  private readonly maxStdoutBytes: number;
  private readonly maxStderrBytes: number;

  constructor(options: PhuxCliOptions = {}) {
    this.executable = options.executable ?? "phux";
    this.socket = options.socket;
    this.cwd = options.cwd;
    this.env = options.env;
    this.runner = options.runner ?? nodeProcessRunner;
    this.maxStdoutBytes = options.maxStdoutBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
    this.maxStderrBytes = options.maxStderrBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
    requireNonNegativeInteger(this.maxStdoutBytes, "maxStdoutBytes");
    requireNonNegativeInteger(this.maxStderrBytes, "maxStderrBytes");
  }

  async probe(options: ExecutionOptions = {}): Promise<PhuxProbe> {
    try {
      const result = await this.execute(["--version"], options);
      if (result.termination !== "completed") this.throwTermination(result, [this.executable, "--version"]);
      if (result.exitCode !== 0) {
        return { available: false, reason: failureMessage(result) };
      }
      const rawVersion = result.stdout.trim();
      const match = VERSION_PATTERN.exec(rawVersion);
      if (match === null || match[1] === undefined) {
        return {
          available: false,
          rawVersion,
          reason: `unexpected version output; expected "phux X.Y.Z", got ${JSON.stringify(rawVersion)}`,
        };
      }
      const version = match[1];
      if (!isCompatiblePhuxVersion(version)) {
        return {
          available: false,
          version,
          rawVersion,
          reason: `@phux/pi requires phux >= ${MINIMUM_PHUX_VERSION}; found ${version}`,
        };
      }
      return { available: true, version, rawVersion };
    } catch (error) {
      if (error instanceof PhuxError &&
          (error.code === "aborted" || error.code === "timeout" || error.code === "output_limit")) {
        throw error;
      }
      return {
        available: false,
        reason: isMissingExecutable(error)
          ? `phux executable ${JSON.stringify(this.executable)} was not found; install phux or configure its absolute path`
          : error instanceof Error ? error.message : String(error),
      };
    }
  }

  async ls(options: ExecutionOptions = {}): Promise<SessionList> {
    const args = this.withSocket(["ls", "--json"]);
    return this.jsonCommand("ls", args, options, parseSessionList);
  }

  /** Inventory panes and their owning session through the documented agent CLI projection. */
  async agentList(options: ExecutionOptions = {}): Promise<AgentStateList> {
    const args = this.withSocket(["agent", "list", "--json"]);
    return this.jsonCommand("agent list", args, options, parseAgentStateList);
  }

  async create(name: string, options: CreateOptions = {}): Promise<CreateResult> {
    if (name.trim().length === 0) throw new TypeError("name must be non-empty");
    const args = ["new", "--json", "-s", name];
    if (options.cwd !== undefined) args.push("--cwd", options.cwd);
    this.pushSocket(args);
    if (options.command !== undefined) {
      if (options.command.length === 0) throw new TypeError("command must contain at least one argv item");
      args.push("--", ...options.command);
    }
    return this.jsonCommand("new", args, options, parseCreateResult);
  }

  async spawn(options: SpawnOptions = {}): Promise<SpawnResult> {
    validatePlacement(options, true);
    const args = ["spawn", "--json"];
    if (options.satellite !== undefined) args.push("--satellite", options.satellite);
    if (options.target !== undefined) args.push("--target", options.target);
    if (options.split !== undefined) args.push("--split", options.split);
    if (options.ratio !== undefined) args.push("--ratio", String(options.ratio));
    if (options.cwd !== undefined) args.push("--cwd", options.cwd);
    this.pushSocket(args);
    if (options.command !== undefined) {
      if (options.command.length === 0) throw new TypeError("command must contain at least one argv item");
      args.push("--", ...options.command);
    }
    return this.jsonCommand("spawn", args, options, parseSpawnResult);
  }

  async launch(integration: string, options: LaunchOptions = {}): Promise<LaunchResult> {
    if (integration.trim().length === 0) throw new TypeError("integration must be non-empty");
    validatePlacement(options, false);
    const args = ["launch", "--json"];
    if (options.target !== undefined) args.push("--target", options.target);
    if (options.split !== undefined) args.push("--split", options.split);
    if (options.ratio !== undefined) args.push("--ratio", String(options.ratio));
    if (options.cwd !== undefined) args.push("--cwd", options.cwd);
    this.pushSocket(args);
    args.push(integration);
    if (options.extra !== undefined) {
      if (options.extra.length === 0) throw new TypeError("extra must contain at least one argv item");
      args.push("--", ...options.extra);
    }
    return this.jsonCommand("launch", args, options, parseLaunchResult);
  }

  async insertPane(
    target: string,
    newPane: string,
    options: SpatialOptions = {},
  ): Promise<InsertPaneResult> {
    validateSpatial(target, newPane, options);
    const args = ["insert-pane", "--json"];
    pushSpatialGeometry(args, options);
    this.pushSocket(args);
    args.push(target, newPane);
    return this.jsonCommand("insert-pane", args, options, parseInsertPaneResult);
  }

  async movePane(
    source: string,
    target: string,
    options: SpatialOptions = {},
  ): Promise<MovePaneResult> {
    validateSpatial(source, target, options);
    const args = ["move-pane", "--json"];
    pushSpatialGeometry(args, options);
    this.pushSocket(args);
    args.push(source, target);
    return this.jsonCommand("move-pane", args, options, parseMovePaneResult);
  }

  async swapPane(
    first: string,
    second: string,
    options: ExecutionOptions = {},
  ): Promise<SwapPaneResult> {
    validateDistinctTargets(first, second);
    const args = ["swap-pane", "--json"];
    this.pushSocket(args);
    args.push(first, second);
    return this.jsonCommand("swap-pane", args, options, parseSwapPaneResult);
  }

  /** Read one pane's public projection, including declared-record provenance. */
  async agentShow(options: AgentTargetOptions): Promise<AgentStateList> {
    const args = ["agent", "show", "--json"];
    this.pushSocket(args);
    args.push(options.target);
    return this.jsonCommand("agent show", args, options, parseAgentStateList);
  }

  /** Write and parse the CLI's confirmed whole-record response. */
  async agentSet(
    target: string,
    record: AgentRecord,
    options: ExecutionOptions = {},
  ): Promise<AgentRecord> {
    const args = [
      "agent", "set", target,
      "--name", record.name,
      "--kind", record.kind,
      "--state", record.state,
      "--attention", record.attention,
      "--session", record.session,
    ];
    this.pushSocket(args);
    const result = await this.completed("agent set", args, options, false);
    return parseAgentConfirmation("agent set", this.executable, result.stdout, args);
  }

  /** Clear a declaration and require the CLI's confirmed tombstone response. */
  async agentClear(target: string, options: ExecutionOptions = {}): Promise<void> {
    const args = ["agent", "clear", target];
    this.pushSocket(args);
    const result = await this.completed("agent clear", args, options, false);
    if (!/^@\d+\t-$/.test(result.stdout.trim())) {
      throw invalidResponse("agent clear", this.executable, args, "expected @N\\t- confirmation");
    }
  }

  async renderedSnapshot(options: RenderedSnapshotOptions): Promise<RenderedFrame> {
    requirePositiveInteger(options.cols, "cols");
    requirePositiveInteger(options.rows, "rows");
    const args = ["snapshot", "--rendered", "--json", "--cols", String(options.cols), "--rows", String(options.rows)];
    this.pushSocket(args);
    if (options.session !== undefined) args.push(options.session);
    return this.jsonCommand("snapshot --rendered", args, options, parseRenderedFrame);
  }

  async snapshot(options: SnapshotOptions = {}): Promise<ScreenState> {
    const args = ["snapshot", "--json"];
    if (options.scrollback === true) args.push("--scrollback");
    else if (typeof options.scrollback === "number") {
      requireNonNegativeInteger(options.scrollback, "scrollback");
      args.push("--scrollback", String(options.scrollback));
    }
    if (options.cells === true) args.push("--cells");
    this.pushSocket(args);
    if (options.target !== undefined) args.push(options.target);
    return this.jsonCommand("snapshot", args, options, parseScreenState);
  }

  async wait(options: WaitOptions = {}): Promise<WaitOutcome> {
    const args = ["wait", "--json"];
    if (options.until !== undefined) args.push("--until", options.until);
    if (options.idleMs !== undefined) {
      requireNonNegativeInteger(options.idleMs, "idleMs");
      args.push("--idle", String(options.idleMs));
    }
    if (options.phuxTimeoutSeconds !== undefined) {
      requireNonNegativeInteger(options.phuxTimeoutSeconds, "phuxTimeoutSeconds");
      args.push("--timeout", String(options.phuxTimeoutSeconds));
    }
    this.pushSocket(args);
    if (options.target !== undefined) args.push(options.target);
    const result = await this.completed("wait", args, options, true);
    if (result.exitCode !== 0 && result.exitCode !== 124) {
      throw commandFailed("wait", this.executable, args, result);
    }
    const screen = parseJson("wait", this.executable, result.stdout, args, parseScreenState);
    return { outcome: result.exitCode === 124 ? "timed_out" : "satisfied", screen };
  }

  async run(target: string, command: readonly string[], options: RunOptions = {}): Promise<RunResult> {
    if (command.length === 0) throw new TypeError("command must contain at least one argv item");
    const args = ["run", "--json"];
    if (options.phuxTimeoutSeconds !== undefined) {
      requireNonNegativeInteger(options.phuxTimeoutSeconds, "phuxTimeoutSeconds");
      args.push("--timeout", String(options.phuxTimeoutSeconds));
    }
    this.pushSocket(args);
    args.push(target, ...command);

    const result = await this.completed("run", args, options, true);
    if (result.exitCode !== 0 && result.stdout.trim().length === 0) {
      throw commandFailed("run", this.executable, args, result);
    }
    const parsed = parseJson("run", this.executable, result.stdout, args, parseRunResult);
    const expectedExit = parsed.exit_code >= 0 && parsed.exit_code <= 255 ? parsed.exit_code : 255;
    if (result.exitCode !== expectedExit) {
      throw invalidResponse(
        "run",
        this.executable,
        args,
        `$.exit_code (${parsed.exit_code}) does not match process exit ${String(result.exitCode)}`,
      );
    }
    return parsed;
  }

  async sendKeys(target: string, keys: readonly string[], options: ExecutionOptions = {}): Promise<void> {
    if (keys.length === 0) throw new TypeError("keys must contain at least one item");
    const args = ["send-keys"];
    this.pushSocket(args);
    args.push(target, ...keys);
    await this.completed("send-keys", args, options, false);
  }

  async kill(target: string, options: ExecutionOptions = {}): Promise<void> {
    const args = ["kill", target];
    this.pushSocket(args);
    await this.completed("kill", args, options, false);
  }

  async signal(target: string, signal: TerminalSignal, options: ExecutionOptions = {}): Promise<void> {
    const args = ["signal", target, signal];
    this.pushSocket(args);
    await this.completed("signal", args, options, false);
  }

  async tag(action: TagAction, target: string, tags: readonly string[] = [], options: ExecutionOptions = {}): Promise<readonly TagRow[]> {
    if (action !== "ls" && tags.length === 0) throw new TypeError("tags must contain at least one item");
    if (action === "ls" && tags.length !== 0) throw new TypeError("tag ls does not accept tags");
    const args = ["tag", action, target, ...tags];
    this.pushSocket(args);
    const result = await this.completed(`tag ${action}`, args, options, false);
    return parseTagRows(result.stdout, `tag ${action}`, this.executable, args);
  }

  async ask(target: string, question: string, options: AskOptions = {}): Promise<AskedEvent> {
    if (question.trim().length === 0) throw new TypeError("question must be non-empty");
    const args = ["ask", target, "--json"];
    if (options.id !== undefined) args.push("--id", options.id);
    for (const suggestion of options.suggestions ?? []) args.push("--suggest", suggestion);
    if (options.elapsedSeconds !== undefined) {
      requireNonNegativeInteger(options.elapsedSeconds, "elapsedSeconds");
      args.push("--elapsed-seconds", String(options.elapsedSeconds));
    }
    this.pushSocket(args);
    args.push(question);
    return this.jsonCommand("ask", args, options, parseAskedEvent);
  }

  async watch(options: WatchOptions): Promise<WatchCollection> {
    requirePositiveInteger(options.durationMs, "durationMs");
    requirePositiveInteger(options.maxEvents, "maxEvents");
    const args = ["watch", "--json"];
    this.pushSocket(args);
    args.push(options.target);
    let result: ProcessResult;
    try {
      result = await this.execute(args, { ...options, timeoutMs: options.durationMs });
    } catch (cause) {
      throw new PhuxError("unavailable", `could not start phux executable ${JSON.stringify(this.executable)}: ${errorText(cause)}`, {
        argv: [this.executable, ...args], cause,
      });
    }
    if (result.termination === "aborted") this.throwTermination(result, [this.executable, ...args]);
    if (result.termination === "output_limit") this.throwTermination(result, [this.executable, ...args]);
    if (result.termination === "completed" && result.exitCode !== 0) {
      throw commandFailed("watch", this.executable, args, result);
    }
    const events = parseWatchLines(result.stdout, this.executable, args);
    const truncated = events.length > options.maxEvents;
    return {
      events: truncated ? events.slice(-options.maxEvents) : events,
      truncated,
      ended: result.termination === "completed",
    };
  }

  private async jsonCommand<T>(
    verb: string,
    args: string[],
    options: ExecutionOptions,
    parser: (value: unknown) => T,
  ): Promise<T> {
    const result = await this.completed(verb, args, options, false);
    return parseJson(verb, this.executable, result.stdout, args, parser);
  }

  private async completed(
    verb: string,
    args: string[],
    options: ExecutionOptions,
    allowNonzero: boolean,
  ): Promise<ProcessResult> {
    let result: ProcessResult;
    try {
      result = await this.execute(args, options);
    } catch (cause) {
      const message = isMissingExecutable(cause)
        ? `phux executable ${JSON.stringify(this.executable)} was not found; install phux or configure its absolute path`
        : `could not start phux executable ${JSON.stringify(this.executable)}: ${errorText(cause)}`;
      throw new PhuxError("unavailable", message, {
        argv: [this.executable, ...args],
        cause,
      });
    }
    this.throwTermination(result, [this.executable, ...args]);
    if (!allowNonzero && result.exitCode !== 0) {
      throw commandFailed(verb, this.executable, args, result);
    }
    return result;
  }

  private execute(args: string[], options: ExecutionOptions): Promise<ProcessResult> {
    const request: RunRequest = {
      executable: this.executable,
      args,
      ...(this.cwd === undefined ? {} : { cwd: this.cwd }),
      ...(this.env === undefined ? {} : { env: this.env }),
      ...(options.signal === undefined ? {} : { signal: options.signal }),
      ...(options.timeoutMs === undefined ? {} : { timeoutMs: options.timeoutMs }),
      maxStdoutBytes: this.maxStdoutBytes,
      maxStderrBytes: this.maxStderrBytes,
    };
    return this.runner(request);
  }

  private throwTermination(result: ProcessResult, argv: readonly string[]): void {
    if (result.termination === "aborted") {
      throw new PhuxError("aborted", "phux command was aborted", { argv, stderr: result.stderr });
    }
    if (result.termination === "timed_out") {
      throw new PhuxError("timeout", "phux command exceeded its local subprocess timeout", {
        argv,
        stderr: result.stderr,
      });
    }
    if (result.termination === "output_limit") {
      const limit = result.outputLimit === "stdout" ? this.maxStdoutBytes : this.maxStderrBytes;
      throw new PhuxError(
        "output_limit",
        `phux command exceeded the ${String(limit)}-byte ${result.outputLimit} capture limit`,
        { argv, stderr: result.stderr },
      );
    }
  }

  private withSocket(args: string[]): string[] {
    this.pushSocket(args);
    return args;
  }

  private pushSocket(args: string[]): void {
    if (this.socket !== undefined) args.push("--socket", this.socket);
  }
}

function parseJson<T>(
  verb: string,
  executable: string,
  stdout: string,
  args: string[],
  parser: (value: unknown) => T,
): T {
  let value: unknown;
  try {
    value = JSON.parse(stdout);
  } catch (cause) {
    throw new PhuxError("malformed_json", `phux ${verb} returned malformed JSON: ${errorText(cause)}`, {
      argv: [executable, ...args],
      cause,
    });
  }
  try {
    return parser(value);
  } catch (cause) {
    if (cause instanceof SchemaValidationError) {
      throw invalidResponse(verb, executable, args, cause.message, cause);
    }
    throw cause;
  }
}

function parseAgentConfirmation(
  verb: string,
  executable: string,
  stdout: string,
  args: string[],
): AgentRecord {
  const line = stdout.trim();
  const tab = line.indexOf("\t");
  if (tab < 2 || !/^@\d+$/.test(line.slice(0, tab))) {
    throw invalidResponse(verb, executable, args, "expected @N\\t<record-json> confirmation");
  }
  return parseJson(verb, executable, line.slice(tab + 1), args, parseAgentRecord);
}

function parseWatchLines(stdout: string, executable: string, args: string[]): WatchEvent[] {
  const lines = stdout.split("\n").filter((line) => line.trim().length > 0);
  return lines.map((line, index) => {
    let value: unknown;
    try {
      value = JSON.parse(line);
    } catch (cause) {
      throw new PhuxError("malformed_json", `phux watch returned malformed JSON on line ${String(index + 1)}: ${errorText(cause)}`, {
        argv: [executable, ...args], cause,
      });
    }
    try {
      return parseWatchEvent(value, `$[${index}] (phux watch --json line)`);
    } catch (cause) {
      if (cause instanceof SchemaValidationError) {
        throw invalidResponse("watch", executable, args, cause.message, cause);
      }
      throw cause;
    }
  });
}

function parseTagRows(stdout: string, verb: string, executable: string, args: string[]): TagRow[] {
  const lines = stdout.trim().length === 0 ? [] : stdout.trim().split("\n");
  if (lines.length === 0) throw invalidResponse(verb, executable, args, "expected at least one @N\\t<tag text> confirmation");
  return lines.map((line) => {
    const tab = line.indexOf("\t");
    const terminal = tab < 0 ? "" : line.slice(0, tab);
    if (!/^@\d+$/.test(terminal)) {
      throw invalidResponse(verb, executable, args, "expected @N\\t<tag text> confirmation");
    }
    return { terminal, tagsText: line.slice(tab + 1) };
  });
}

function invalidResponse(
  verb: string,
  executable: string,
  args: string[],
  detail: string,
  cause?: unknown,
): PhuxError {
  return new PhuxError(
    "invalid_response",
    `phux ${verb} JSON does not match its documented CLI shape: ${detail}`,
    { argv: [executable, ...args], cause },
  );
}

function commandFailed(
  verb: string,
  executable: string,
  args: string[],
  result: ProcessResult,
): PhuxError {
  return new PhuxError(
    "command_failed",
    `phux ${verb} failed with exit code ${String(result.exitCode)}${diagnosticSuffix(result.stderr)}`,
    {
      argv: [executable, ...args],
      exitCode: result.exitCode,
      stderr: result.stderr,
    },
  );
}

function diagnosticSuffix(stderr: string): string {
  const detail = stderr.trim();
  return detail.length === 0 ? "" : `: ${detail}`;
}

function failureMessage(result: ProcessResult): string {
  return `phux --version exited ${String(result.exitCode)}${diagnosticSuffix(result.stderr)}`;
}

function isCompatiblePhuxVersion(version: string): boolean {
  const core = version.split(/[+-]/, 1)[0]?.split(".").map(Number);
  if (core === undefined || core.length !== 3) return false;
  const [major = 0, minor = 0, patch = 0] = core;
  if (major !== 0) return major > 0;
  if (minor !== 1) return minor > 1;
  if (patch !== 0) return patch > 0;
  return !version.includes("-");
}

function isMissingExecutable(error: unknown): boolean {
  return error instanceof Error && "code" in error && error.code === "ENOENT";
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

const SATELLITE_PANE_SELECTOR = /^[^/\s]+\/@\d+$/;

function validatePlacement(options: PlacementOptions & { readonly satellite?: string }, allowSatellite: boolean): void {
  if (options.target === undefined && (options.split !== undefined || options.ratio !== undefined)) {
    throw new TypeError("target is required when split or ratio is provided");
  }
  if (options.target !== undefined) {
    if (options.target.trim().length === 0) throw new TypeError("target must be non-empty");
    if (SATELLITE_PANE_SELECTOR.test(options.target)) {
      throw new TypeError("explicit placement is local-only; satellite pane targets are unsupported");
    }
  }
  if (options.satellite !== undefined) {
    if (!allowSatellite) throw new TypeError("satellite is not supported for launch placement");
    if (options.target !== undefined) throw new TypeError("satellite and target placement cannot be combined");
  }
  if (options.split !== undefined && options.split !== "horizontal" && options.split !== "vertical") {
    throw new TypeError("split must be horizontal or vertical");
  }
  if (options.ratio !== undefined) requireRatio(options.ratio);
}

function validateSpatial(first: string, second: string, options: SpatialOptions): void {
  validateDistinctTargets(first, second);
  if (options.direction !== undefined && options.direction !== "horizontal" && options.direction !== "vertical") {
    throw new TypeError("direction must be horizontal or vertical");
  }
  if (options.ratio !== undefined) requireRatio(options.ratio);
}

function validateDistinctTargets(first: string, second: string): void {
  if (first.trim().length === 0 || second.trim().length === 0) {
    throw new TypeError("spatial targets must be non-empty");
  }
  if (first === second) throw new TypeError("spatial actions require two distinct targets");
  if (SATELLITE_PANE_SELECTOR.test(first) || SATELLITE_PANE_SELECTOR.test(second)) {
    throw new TypeError("spatial actions require local pane targets");
  }
}

function pushSpatialGeometry(args: string[], options: SpatialOptions): void {
  if (options.direction !== undefined) args.push("--split", options.direction);
  if (options.ratio !== undefined) args.push("--ratio", String(options.ratio));
}

function requireRatio(value: number): void {
  if (!Number.isFinite(value) || value <= 0 || value >= 1) {
    throw new RangeError("ratio must be finite and strictly between 0 and 1");
  }
}

function requireNonNegativeInteger(value: number, name: string): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new RangeError(`${name} must be a non-negative safe integer`);
  }
}

function requirePositiveInteger(value: number, name: string): void {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new RangeError(`${name} must be a positive safe integer`);
  }
}
