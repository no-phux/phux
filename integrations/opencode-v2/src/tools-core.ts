type JsonSchema = Readonly<Record<string, unknown>>;

export interface ToolContext {
  readonly sessionID: string;
  readonly agent: string;
  readonly messageID: string;
  readonly id: string;
}

export interface ToolResult {
  readonly content: string;
  readonly metadata: PhuxToolMetadata;
}

export interface PhuxToolDefinition<Input = never> {
  readonly name: string;
  readonly description: string;
  readonly input: JsonSchema;
  readonly execute: (input: Input, context: ToolContext) => Promise<ToolResult>;
}

import { PhuxCli } from "../../runtime/src/adapter.js";
import type { ScreenState } from "../../runtime/src/schemas.js";

const TARGET = stringSchema(1, 512, "Explicit phux target selector; otherwise use this plugin instance's selected target, then PHUX_TARGET");
const LOCAL_TIMEOUT = integerSchema(1, 3_600_000, "Local subprocess timeout in milliseconds");
const RUN_TIMEOUT = integerSchema(0, 86_400, "phux run timeout in seconds; 0 waits indefinitely");
const WAIT_TIMEOUT = integerSchema(1, 86_400, "phux wait timeout in seconds; omit to wait indefinitely");

interface ListInput { readonly local_timeout_ms?: number }
interface CreateInput {
  readonly name: string;
  readonly cwd?: string;
  readonly command?: string[];
  readonly local_timeout_ms?: number;
}
interface SnapshotInput {
  readonly target?: string;
  readonly scrollback?: number;
  readonly cells?: boolean;
  readonly local_timeout_ms?: number;
}
interface SendKeysInput {
  readonly target?: string;
  readonly keys: string[];
  readonly local_timeout_ms?: number;
}
interface RunInput {
  readonly target?: string;
  readonly command: string;
  readonly timeout_seconds?: number;
  readonly local_timeout_ms?: number;
}
interface WaitInput {
  readonly target?: string;
  readonly until?: string;
  readonly idle_ms?: number;
  readonly timeout_seconds?: number;
  readonly local_timeout_ms?: number;
}

export const MAX_MODEL_BYTES = 12 * 1024;
export const MAX_MODEL_LINES = 200;
export const DEFAULT_SHORT_TIMEOUT_MS = 10_000;
const MODEL_TRUNCATION_NOTICE = `[OpenCode adapter truncated terminal output to the last ${String(MAX_MODEL_LINES)} lines within ${String(MAX_MODEL_BYTES)} bytes]`;
const PHUX_TRUNCATION_NOTICE = "[phux reported that terminal output was already truncated]";

export interface PhuxToolMetadata {
  readonly operation: "list" | "create" | "snapshot" | "send_keys" | "run" | "wait";
  readonly target?: string;
  readonly count?: number;
  readonly exitCode?: number;
  readonly durationMs?: number;
  readonly outcome?: string;
  readonly rows?: number;
  readonly cols?: number;
  readonly modelOutputTruncated?: boolean;
  readonly phuxOutputTruncated?: boolean;
}

export interface PhuxToolRuntime {
  readonly cli: PhuxCli;
  readonly environmentTarget?: string;
  getSelectedTarget(): string | undefined;
  selectTarget(target: string): void;
  targetSelected?(context: ToolContext): void;
}

/** Build the six public OpenCode tools around one plugin-instance target selection. */
export function createPhuxTools(runtime: PhuxToolRuntime): Record<string, PhuxToolDefinition<any>> {
  return {
    phux_list: defineTool<ListInput>({
      name: "phux_list",
      description: "List phux sessions. Output is compact and bounded; this never changes phux focus.",
      input: objectSchema({ local_timeout_ms: LOCAL_TIMEOUT }),
      async execute(args, context) {
        const result = await runtime.cli.ls(shortExecution(args.local_timeout_ms, context));
        const lines = result.sessions.map((session) =>
          `${session.name}\twindows=${String(session.windows)}\tattached=${String(session.attached)}`);
        const output = boundedResult(
          `sessions=${String(result.sessions.length)}`,
          lines.join("\n") || "No phux sessions.",
        );
        return resultObject(`${String(result.sessions.length)} phux session(s)`, output.text, {
          operation: "list",
          count: result.sessions.length,
          modelOutputTruncated: output.truncated,
        });
      },
    }),

    phux_create: defineTool<CreateInput>({
      name: "phux_create",
      description: "Create a named phux session without attaching, then select its seed @id for this plugin instance.",
      input: objectSchema({
        name: stringSchema(1, 255),
        cwd: stringSchema(1, 4096),
        command: arraySchema({ type: "string", maxLength: 65_536 }, 1, 256, "Optional command argv; this is an argv array only for session creation"),
        local_timeout_ms: LOCAL_TIMEOUT,
      }, ["name"]),
      async execute(args, context) {
        const created = await runtime.cli.create(args.name, {
          ...(args.cwd === undefined ? {} : { cwd: args.cwd }),
          ...(args.command === undefined ? {} : { command: args.command }),
          ...shortExecution(args.local_timeout_ms, context),
        });
        if (created.session !== args.name) {
          throw new Error(`phux new returned session ${JSON.stringify(created.session)}; expected ${JSON.stringify(args.name)}`);
        }
        const target = `@${String(created.terminal_id)}`;
        runtime.selectTarget(target);
        runtime.targetSelected?.(context);
        return resultObject(`Created ${created.session} at ${target}`, `Created ${created.session} at ${target}; selected it as this plugin instance's default phux target.`, {
          operation: "create",
          target,
        });
      },
    }),

    phux_snapshot: defineTool<SnapshotInput>({
      name: "phux_snapshot",
      description: "Read a phux pane without attaching or resizing. Target resolution is explicit target, selected created target, then PHUX_TARGET. Terminal text is bounded to 200 lines and 12 KiB.",
      input: objectSchema({
        target: TARGET,
        scrollback: integerSchema(0, 100_000),
        cells: { type: "boolean" },
        local_timeout_ms: LOCAL_TIMEOUT,
      }),
      async execute(args, context) {
        const target = resolveTarget(args.target, runtime);
        const screen = await runtime.cli.snapshot({
          target,
          ...(args.scrollback === undefined ? {} : { scrollback: args.scrollback }),
          ...(args.cells === undefined ? {} : { cells: args.cells }),
          ...shortExecution(args.local_timeout_ms, context),
        });
        return screenResult("snapshot", target, screen);
      },
    }),

    phux_send_keys: defineTool<SendKeysInput>({
      name: "phux_send_keys",
      description: "Send named keys or literal key strings to a phux pane. This is not a paste operation and never uses phux focus.",
      input: objectSchema({
        target: TARGET,
        keys: arraySchema(stringSchema(1, 65_536), 1, 256),
        local_timeout_ms: LOCAL_TIMEOUT,
      }, ["keys"]),
      async execute(args, context) {
        const target = resolveTarget(args.target, runtime);
        await runtime.cli.sendKeys(target, args.keys, shortExecution(args.local_timeout_ms, context));
        return resultObject(`Sent keys to ${target}`, `Sent ${String(args.keys.length)} key item(s) to ${target}.`, {
          operation: "send_keys",
          target,
          count: args.keys.length,
        });
      },
    }),

    phux_run: defineTool<RunInput>({
      name: "phux_run",
      description: "Run one shell command string in a phux pane through phux's documented sentinel. The command is passed as one argument. Output is bounded to 200 lines and 12 KiB.",
      input: objectSchema({
        target: TARGET,
        command: stringSchema(1, 65_536, "One shell command line, passed to phux as one argument"),
        timeout_seconds: RUN_TIMEOUT,
        local_timeout_ms: LOCAL_TIMEOUT,
      }, ["command"]),
      async execute(args, context) {
        const target = resolveTarget(args.target, runtime);
        const result = await runtime.cli.run(target, [args.command], {
          ...(args.timeout_seconds === undefined ? {} : { phuxTimeoutSeconds: args.timeout_seconds }),
          ...longExecution(args.local_timeout_ms, context),
        });
        const output = boundedResult(
          `run exit=${String(result.exit_code)} duration_ms=${String(result.duration_ms)} target=${target}`,
          result.output,
          result.truncated,
        );
        return resultObject(`phux run exited ${String(result.exit_code)}`, output.text, {
          operation: "run",
          target,
          exitCode: result.exit_code,
          durationMs: result.duration_ms,
          modelOutputTruncated: output.truncated,
          phuxOutputTruncated: result.truncated,
        });
      },
    }),

    phux_wait: defineTool<WaitInput>({
      name: "phux_wait",
      description: "Wait for visible text or terminal idleness and return the bounded final screen. until and idle_ms are exclusive; omit both and timeout_seconds to wait indefinitely.",
      input: objectSchema({
        target: TARGET,
        until: stringSchema(1, 4096),
        idle_ms: integerSchema(0, 86_400_000),
        timeout_seconds: WAIT_TIMEOUT,
        local_timeout_ms: LOCAL_TIMEOUT,
      }),
      async execute(args, context) {
        if (args.until !== undefined && args.idle_ms !== undefined) {
          throw new Error("phux_wait accepts either until or idle_ms, not both");
        }
        const target = resolveTarget(args.target, runtime);
        const result = await runtime.cli.wait({
          target,
          ...(args.until === undefined ? {} : { until: args.until }),
          ...(args.idle_ms === undefined ? {} : { idleMs: args.idle_ms }),
          ...(args.timeout_seconds === undefined ? {} : { phuxTimeoutSeconds: args.timeout_seconds }),
          ...longExecution(args.local_timeout_ms, context),
        });
        return screenResult("wait", target, result.screen, result.outcome);
      },
    }),
  };
}

export function resolveTarget(explicit: string | undefined, runtime: Pick<PhuxToolRuntime, "getSelectedTarget" | "environmentTarget">): string {
  if (explicit !== undefined) return explicit;
  const selected = runtime.getSelectedTarget();
  if (selected !== undefined) return selected;
  if (runtime.environmentTarget !== undefined) return runtime.environmentTarget;
  throw new Error("No phux target is available. Pass target explicitly, create a session with phux_create, or set PHUX_TARGET.");
}

function shortExecution(localTimeoutMs: number | undefined, context: ToolContext) {
  return { timeoutMs: localTimeoutMs ?? DEFAULT_SHORT_TIMEOUT_MS };
}

function longExecution(localTimeoutMs: number | undefined, context: ToolContext) {
  return {
    ...(localTimeoutMs === undefined ? {} : { timeoutMs: localTimeoutMs }),
  };
}

function screenResult(
  operation: "snapshot" | "wait",
  target: string,
  screen: ScreenState,
  outcome?: string,
): ToolResult {
  const terminal = [...screen.scrollback, ...screen.lines].join("\n");
  const header = `${operation}${outcome === undefined ? "" : ` ${outcome}`} target=${target} pane=@${String(screen.pane)} size=${String(screen.cols)}x${String(screen.rows)}`;
  const output = boundedResult(header, terminal);
  return resultObject(`${operation}${outcome === undefined ? "" : ` ${outcome}`} on ${target}`, output.text, {
    operation,
    target,
    ...(outcome === undefined ? {} : { outcome }),
    rows: screen.rows,
    cols: screen.cols,
    modelOutputTruncated: output.truncated,
  });
}

function resultObject(title: string, output: string, metadata: PhuxToolMetadata): ToolResult {
  return { content: `${title}\n${output}`, metadata };
}

function defineTool<Input>(definition: PhuxToolDefinition<Input>): PhuxToolDefinition<Input> {
  return definition;
}

function objectSchema(properties: Record<string, JsonSchema>, required: string[] = []): JsonSchema {
  return {
    type: "object",
    properties,
    ...(required.length === 0 ? {} : { required }),
    additionalProperties: false,
  };
}

function stringSchema(minLength: number, maxLength: number, description?: string): JsonSchema {
  return {
    type: "string",
    minLength,
    maxLength,
    pattern: "\\S",
    ...(description === undefined ? {} : { description }),
  };
}

function integerSchema(minimum: number, maximum: number, description?: string): JsonSchema {
  return {
    type: "integer",
    minimum,
    maximum,
    ...(description === undefined ? {} : { description }),
  };
}

function arraySchema(items: JsonSchema, minItems: number, maxItems: number, description?: string): JsonSchema {
  return {
    type: "array",
    items,
    minItems,
    maxItems,
    ...(description === undefined ? {} : { description }),
  };
}

/** Bound terminal body text while preserving the header and explicit notices. */
export function boundedResult(header: string, body: string, phuxTruncated = false): { readonly text: string; readonly truncated: boolean } {
  const fixedNotices = phuxTruncated ? [PHUX_TRUNCATION_NOTICE] : [];
  // Reserve the model notice even when it may not be needed so a truncated result
  // can never exceed the advertised byte or line cap after adding that notice.
  const reserved = [MODEL_TRUNCATION_NOTICE, ...fixedNotices];
  const bodyBytes = Math.max(0, MAX_MODEL_BYTES - byteLength([header, ...reserved].join("\n")) - 1);
  const bodyLines = Math.max(0, MAX_MODEL_LINES - 1 - reserved.length);
  const truncatedBody = truncateTail(body, bodyBytes, bodyLines);
  const notices = [...(truncatedBody.truncated ? [MODEL_TRUNCATION_NOTICE] : []), ...fixedNotices];
  return {
    text: [header, ...(truncatedBody.text.length === 0 ? [] : [truncatedBody.text]), ...notices].join("\n"),
    truncated: truncatedBody.truncated,
  };
}

function truncateTail(input: string, maxBytes: number, maxLines: number): { readonly text: string; readonly truncated: boolean } {
  const lines = input.split("\n");
  let truncated = lines.length > maxLines;
  let text = (truncated ? lines.slice(lines.length - maxLines) : lines).join("\n");
  if (byteLength(text) <= maxBytes) return { text, truncated };
  truncated = true;
  const chars = Array.from(text);
  let used = 0;
  let start = chars.length;
  while (start > 0) {
    const size = byteLength(chars[start - 1] ?? "");
    if (used + size > maxBytes) break;
    used += size;
    start -= 1;
  }
  text = chars.slice(start).join("");
  return { text, truncated };
}

function byteLength(text: string): number {
  return Buffer.byteLength(text, "utf8");
}
