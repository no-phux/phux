import { PhuxError } from "./errors.js";
import { DEFAULT_MAX_OUTPUT_BYTES, nodeProcessRunner, type ProcessResult, type ProcessRunner } from "./runner.js";
import { parseScreenState, SchemaValidationError, type CursorState } from "./schemas.js";

/**
 * Watching a pane for a viewer: the server's styled rendering of its screen,
 * and a resumable wait for the next change. Kept beside `PhuxCli`, which
 * drives panes, because a viewer only reads.
 */

/** `phux snapshot --format html --json`: the server's styled rendering of history plus viewport. */
export interface RenderedScreen {
  readonly pane: number;
  readonly cols: number;
  readonly rows: number;
  readonly cursor: CursorState | null;
  readonly title: string | null;
  /** Inline-styled markup from libghostty's Formatter. Untrusted: never assign it as HTML. */
  readonly html: string;
}

export interface WatchUntilOutcome {
  readonly outcome: "matched" | "timed_out";
  /** Name of the event that ended the watch; the stream's vocabulary grows, so it is not narrowed. */
  readonly event?: string;
  /** Where the watch stopped; pass it as `after` so the next watch misses nothing. */
  readonly cursor?: string;
}

export interface PhuxScreenOptions {
  readonly executable?: string;
  readonly socket?: string;
  readonly cwd?: string;
  readonly env?: NodeJS.ProcessEnv;
  readonly runner?: ProcessRunner;
  readonly maxStdoutBytes?: number;
}

interface CallOptions {
  readonly signal?: AbortSignal;
  /** Kill the local subprocess after this many ms. */
  readonly timeoutMs?: number;
}

export class PhuxScreen {
  readonly executable: string;
  readonly socket: string | undefined;
  private readonly cwd: string | undefined;
  private readonly env: NodeJS.ProcessEnv | undefined;
  private readonly runner: ProcessRunner;
  private readonly maxStdoutBytes: number;

  constructor(options: PhuxScreenOptions = {}) {
    this.executable = options.executable ?? "phux";
    this.socket = options.socket;
    this.cwd = options.cwd;
    this.env = options.env;
    this.runner = options.runner ?? nodeProcessRunner;
    this.maxStdoutBytes = options.maxStdoutBytes ?? DEFAULT_MAX_OUTPUT_BYTES;
  }

  /** The pane rendered by the server's Formatter: `history` rows above the viewport, which is always whole. */
  async rendered(target: string, options: CallOptions & { readonly history?: number } = {}): Promise<RenderedScreen> {
    const args = ["snapshot", "--json", "--format", "html"];
    if (options.history !== undefined) {
      if (!Number.isSafeInteger(options.history) || options.history < 0) throw new RangeError("history must be a non-negative integer");
      args.push("--tail", String(options.history));
    }
    this.pushSocket(args);
    args.push(target);
    const result = await this.run("snapshot --format html", args, options.timeoutMs ?? 10_000, options.signal);
    if (result.exitCode !== 0) throw this.failed("snapshot --format html", args, result);
    let value: unknown;
    try {
      value = JSON.parse(result.stdout);
    } catch (cause) {
      throw new PhuxError("malformed_json", "phux snapshot --format html returned malformed JSON", { argv: this.argv(args), cause });
    }
    try {
      return parseRenderedScreen(value);
    } catch (cause) {
      if (!(cause instanceof SchemaValidationError)) throw cause;
      throw new PhuxError("invalid_response", `phux snapshot --format html JSON does not match its documented CLI shape: ${cause.message}`, {
        argv: this.argv(args),
        cause,
      });
    }
  }

  /**
   * Block until the pane emits one of `until`, resuming after `after`. Exit
   * 124 is a quiet pane, not a failure, and still reports a cursor.
   */
  async watchUntil(
    target: string,
    options: CallOptions & { readonly until: readonly string[]; readonly after?: string; readonly timeoutSeconds: number },
  ): Promise<WatchUntilOutcome> {
    if (options.until.length === 0) throw new TypeError("until must name at least one event");
    if (!Number.isSafeInteger(options.timeoutSeconds) || options.timeoutSeconds < 1 || options.timeoutSeconds > 86_400) {
      throw new RangeError("timeoutSeconds must be an integer from 1 through 86400");
    }
    const args = ["watch", "--json"];
    for (const event of options.until) args.push("--until", event);
    args.push("--timeout", String(options.timeoutSeconds));
    if (options.after !== undefined) args.push("--after", options.after);
    this.pushSocket(args);
    args.push(target);
    const result = await this.run("watch", args, options.timeoutMs ?? options.timeoutSeconds * 1000 + 5000, options.signal);
    if (result.exitCode !== 0 && result.exitCode !== 124) throw this.failed("watch", args, result);
    const event = lastStringField(result.stdout, "event");
    const cursor = lastStringField(result.stderr, "cursor");
    return {
      outcome: result.exitCode === 0 ? "matched" : "timed_out",
      ...(event === undefined ? {} : { event }),
      ...(cursor === undefined ? {} : { cursor }),
    };
  }

  private async run(verb: string, args: string[], timeoutMs: number, signal: AbortSignal | undefined): Promise<ProcessResult> {
    let result: ProcessResult;
    try {
      result = await this.runner({
        executable: this.executable,
        args,
        ...(this.cwd === undefined ? {} : { cwd: this.cwd }),
        ...(this.env === undefined ? {} : { env: this.env }),
        ...(signal === undefined ? {} : { signal }),
        timeoutMs,
        maxStdoutBytes: this.maxStdoutBytes,
        maxStderrBytes: DEFAULT_MAX_OUTPUT_BYTES,
      });
    } catch (cause) {
      const missing = cause instanceof Error && "code" in cause && cause.code === "ENOENT";
      throw new PhuxError("unavailable", missing
        ? `phux executable ${JSON.stringify(this.executable)} was not found; install phux or configure its absolute path`
        : `could not start phux executable ${JSON.stringify(this.executable)}: ${cause instanceof Error ? cause.message : String(cause)}`,
      { argv: this.argv(args), cause });
    }
    if (result.termination === "aborted") throw new PhuxError("aborted", `phux ${verb} was aborted`, { argv: this.argv(args) });
    if (result.termination === "timed_out") throw new PhuxError("timeout", `phux ${verb} exceeded its local subprocess timeout`, { argv: this.argv(args) });
    if (result.termination === "output_limit") throw new PhuxError("output_limit", `phux ${verb} exceeded its ${result.outputLimit} capture limit`, { argv: this.argv(args) });
    return result;
  }

  private failed(verb: string, args: string[], result: ProcessResult): PhuxError {
    let cliError: Record<string, unknown> | undefined;
    try {
      const document: unknown = JSON.parse(result.stderr.trim());
      if (document !== null && typeof document === "object" && !Array.isArray(document)) cliError = document as Record<string, unknown>;
    } catch {
      // Older binaries print plain diagnostics; stderr still carries them.
    }
    const detail = result.stderr.trim();
    return new PhuxError("command_failed", `phux ${verb} failed with exit code ${String(result.exitCode)}${detail === "" ? "" : `: ${detail}`}`, {
      argv: this.argv(args),
      exitCode: result.exitCode,
      stderr: result.stderr,
      ...(cliError === undefined ? {} : { cliError }),
    });
  }

  private pushSocket(args: string[]): void {
    if (this.socket !== undefined) args.push("--socket", this.socket);
  }

  private argv(args: readonly string[]): string[] {
    return [this.executable, ...args];
  }
}

export function parseRenderedScreen(value: unknown): RenderedScreen {
  const screen = parseScreenState(value);
  const root = value as Record<string, unknown>;
  const rendered = root.rendered;
  if (rendered === null || typeof rendered !== "object" || Array.isArray(rendered)) {
    throw new SchemaValidationError("$.rendered", "an object");
  }
  const { format, data } = rendered as Record<string, unknown>;
  if (format !== "html") throw new SchemaValidationError("$.rendered.format", '"html"');
  if (typeof data !== "string") throw new SchemaValidationError("$.rendered.data", "a string");
  const title = root.title;
  if (title !== undefined && title !== null && typeof title !== "string") throw new SchemaValidationError("$.title", "a string or null");
  return { pane: screen.pane, cols: screen.cols, rows: screen.rows, cursor: screen.cursor, title: title ?? null, html: data };
}

/** The named string field of the last JSON line that has it: `phux watch` events and its cursor report. */
function lastStringField(text: string, field: string): string | undefined {
  const lines = text.split("\n").map((line) => line.trim()).filter((line) => line.startsWith("{"));
  for (const line of lines.reverse()) {
    try {
      const value: unknown = JSON.parse(line);
      if (value !== null && typeof value === "object" && typeof (value as Record<string, unknown>)[field] === "string") {
        return (value as Record<string, string>)[field];
      }
    } catch {
      // Not a JSON document line.
    }
  }
  return undefined;
}
