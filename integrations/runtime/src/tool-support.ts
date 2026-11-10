import type { ExecutionOptions, PhuxCli } from "./adapter.js";

/** The host-independent tool contract and the helpers every tool module shares. */
export type JsonSchema = Readonly<Record<string, unknown>>;
export interface ToolContext {
  readonly sessionID: string;
  readonly agent: string;
  readonly messageID: string;
  readonly id: string;
  readonly signal?: AbortSignal;
}
export interface PhuxToolMetadata {
  readonly operation: string;
  readonly target?: string;
  readonly modelOutputTruncated?: boolean;
  readonly [key: string]: unknown;
}
/** A base64 image for hosts that pass images to the model beside the text. */
export interface ToolImage {
  readonly data: string;
  readonly mimeType: "image/png" | "image/jpeg";
}
export interface ToolResult {
  readonly content: string;
  readonly metadata: PhuxToolMetadata;
  readonly images?: readonly ToolImage[];
}
export interface PhuxToolDefinition<Input = never> {
  readonly name: string;
  readonly description: string;
  readonly input: JsonSchema;
  readonly execute: (input: Input, context: ToolContext) => Promise<ToolResult>;
}
export interface PhuxToolRuntime {
  readonly cli: PhuxCli;
  readonly environmentTarget?: string;
  readonly parentTarget?: string;
  getSelectedTarget(context?: ToolContext): string | undefined;
  selectTarget(target: string, context?: ToolContext): void;
  targetSelected?(context: ToolContext): void;
}

export const MAX_MODEL_BYTES = 12 * 1024;
export const MAX_MODEL_LINES = 200;
export const DEFAULT_SHORT_TIMEOUT_MS = 10_000;
export const TARGET = { type: "string", minLength: 2, maxLength: 512, pattern: "^(?:[^/\\s]+/)?@[0-9]+$", description: "Exact @N or host/@N from phux_panes; otherwise the session-selected target. Never human focus." };
export const STRING = { type: "string", minLength: 1, maxLength: 65_536 };
export const LOCAL_TIMEOUT = { type: "integer", minimum: 1, maximum: 86_405_000, description: "Local deadline in milliseconds. Cancellation stops observation, not the terminal process or already-sent input." };
export const TIMEOUT = { type: "integer", minimum: 1, maximum: 86_400, description: "Finite observation deadline in seconds; default 30. Timeout is not completion." };

export interface Input {
  readonly target?: string;
  readonly local_timeout_ms?: number;
  readonly timeout_seconds?: number;
}

export function resolveTarget(explicit: string | undefined, runtime: Pick<PhuxToolRuntime, "getSelectedTarget" | "environmentTarget">, context?: ToolContext): string {
  const target = explicit ?? runtime.getSelectedTarget(context) ?? runtime.environmentTarget;
  if (target === undefined) throw new Error("No phux target. Use phux_panes and pass an exact target, or spawn a terminal when a pane is needed.");
  return exactTarget(target);
}

export function exactTarget(target: string): string {
  if (!/^(?:[^/\s]+\/)?@\d+$/.test(target)) throw new Error("Use an exact @N or host/@N from phux_panes; session, tag and focus selectors are not safe control targets.");
  return target;
}

export function writeTarget(explicit: string | undefined, runtime: PhuxToolRuntime, context: ToolContext): string {
  const target = resolveTarget(explicit, runtime, context);
  const normalized = target.replace(/@(0+)(?=\d)/, "@");
  const parent = runtime.parentTarget?.replace(/@(0+)(?=\d)/, "@");
  if (parent !== undefined && normalized === parent) throw new Error("Refusing input into the pane hosting this agent. Use another exact target for phux input; ordinary commands belong in the built-in shell.");
  return target;
}

export function execution(input: Input, context: ToolContext): ExecutionOptions {
  const timeoutMs = input.local_timeout_ms ?? DEFAULT_SHORT_TIMEOUT_MS;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 86_405_000) throw new RangeError("local_timeout_ms must be between 1 and 86405000");
  return { timeoutMs, ...(context.signal === undefined ? {} : { signal: context.signal }) };
}

export function operation(input: Input, context: ToolContext) {
  const seconds = input.timeout_seconds ?? 30;
  if (!Number.isSafeInteger(seconds) || seconds < 1 || seconds > 86_400) throw new RangeError("timeout_seconds must be between 1 and 86400");
  return { ...execution({ ...input, local_timeout_ms: input.local_timeout_ms ?? seconds * 1000 + 5000 }, context), phuxTimeoutSeconds: seconds };
}

export function schema(properties: Record<string, JsonSchema>, required: string[] = []): JsonSchema {
  return { type: "object", properties, required, additionalProperties: false };
}

export function documentResult(operation: string, document: object, target?: string): ToolResult {
  const output = boundedResult(`${operation}${target === undefined ? "" : ` target=${target}`}`, JSON.stringify(document, null, 2));
  // Large inventory/diagnostic documents must not bypass the model cap through metadata.
  return { content: output.text, metadata: { operation, ...(target === undefined ? {} : { target }), modelOutputTruncated: output.truncated, ...(output.truncated ? {} : { result: document }) } };
}

/** Bound terminal text by UTF-8 bytes and lines; preserve the result header and truncation notices. */
export function boundedResult(header: string, body: string, phuxTruncated = false): { readonly text: string; readonly truncated: boolean } {
  const notice = "[phux adapter truncated output; request a narrower observation]";
  const phuxNotice = "[phux reported that terminal output was already truncated]";
  const suffix = phuxTruncated ? `\n${phuxNotice}` : "";
  const budget = Math.max(0, MAX_MODEL_BYTES - Buffer.byteLength(header + suffix + notice, "utf8") - 3);
  const lines = body.split("\n");
  const lineBudget = Math.max(0, MAX_MODEL_LINES - header.split("\n").length - 3);
  let text = lines.slice(-lineBudget).join("\n");
  let truncated = lines.length > lineBudget;
  const bytes = Buffer.from(text, "utf8");
  if (bytes.length > budget) {
    let start = bytes.length - budget;
    while (start < bytes.length && (bytes[start]! & 0xc0) === 0x80) start++;
    text = bytes.subarray(start).toString("utf8");
    truncated = true;
  }
  return { text: `${header}\n${text}${truncated ? `\n${notice}` : ""}${suffix}`, truncated };
}

/** Bound inventory rows from the start: the header and the first rows carry the structure, unlike terminal text. */
export function headBoundedLines(header: string, rows: readonly string[]): { readonly text: string; readonly truncated: boolean } {
  const notice = (kept: number) => `[phux adapter truncated the list after ${kept} of ${rows.length} rows]`;
  const budget = MAX_MODEL_BYTES - Buffer.byteLength(header + notice(rows.length), "utf8") - 2;
  const lineBudget = MAX_MODEL_LINES - 2;
  let bytes = 0;
  let kept = 0;
  for (const row of rows) {
    const size = Buffer.byteLength(row, "utf8") + 1;
    if (kept >= lineBudget || bytes + size > budget) break;
    bytes += size;
    kept++;
  }
  const truncated = kept < rows.length;
  return { text: [header, ...rows.slice(0, kept), ...(truncated ? [notice(kept)] : [])].join("\n"), truncated };
}
