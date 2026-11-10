import {
  truncateHead,
  truncateTail,
  type AgentToolResult,
  type Theme,
} from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";

import type { PhuxTargetSelection } from "./target-store.js";

/** Result bounding, rendering and deadlines shared by every Pi phux tool module. */
export const MAX_MODEL_BYTES = 12 * 1024;
export const MAX_MODEL_LINES = 200;
const MODEL_TRUNCATION_NOTICE = `[Pi adapter truncated terminal output to the last ${String(MAX_MODEL_LINES)} lines within ${String(MAX_MODEL_BYTES)} bytes]`;
const PHUX_TRUNCATION_NOTICE = "[phux reported that terminal output was already truncated]";

export interface PhuxToolDetails {
  readonly operation: "list" | "panes" | "create" | "spawn" | "launch" | "insert_pane" | "move_pane" | "swap_pane" | "snapshot" | "rendered_snapshot" | "send_keys" | "paste" | "run" | "wait" | "agent_prompt" | "agent_wait" | "resource_wait" | "status" | "runtime_info" | "kill" | "signal" | "tag" | "ask" | "watch" | "targets" | "documents" | "tree_snapshot" | "tree_wait" | "surface_snapshot";
  readonly summary: string;
  readonly target?: string;
  readonly selection?: PhuxTargetSelection;
  readonly count?: number;
  readonly exitCode?: number;
  readonly durationMs?: number;
  readonly outcome?: string;
  readonly result?: Readonly<Record<string, unknown>>;
  readonly warning?: string;
  readonly rows?: number;
  readonly cols?: number;
  readonly modelOutputTruncated?: boolean;
  readonly phuxOutputTruncated?: boolean;
}

export function execution(params: { readonly local_timeout_ms?: number }, signal: AbortSignal | undefined, defaultMs = 10_000) {
  signal?.throwIfAborted();
  const timeoutMs = params.local_timeout_ms ?? defaultMs;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 86_405_000) {
    throw new RangeError("local_timeout_ms must be an integer from 1 through 86405000");
  }
  return { ...(signal === undefined ? {} : { signal }), timeoutMs };
}

/** Bound only terminal body text so the result header and truncation notices remain visible. */
export function boundedResult(
  header: string,
  body: string,
  phuxTruncated = false,
): { readonly text: string; readonly truncated: boolean } {
  const reservedNotices = [MODEL_TRUNCATION_NOTICE, ...(phuxTruncated ? [PHUX_TRUNCATION_NOTICE] : [])];
  const separators = 1 + reservedNotices.length;
  const reservedBytes = Buffer.byteLength(header) +
    reservedNotices.reduce((total, notice) => total + Buffer.byteLength(notice), 0) + separators;
  const bodyResult = truncateTail(body, {
    maxBytes: Math.max(0, MAX_MODEL_BYTES - reservedBytes),
    maxLines: Math.max(0, MAX_MODEL_LINES - 1 - reservedNotices.length),
  });
  const notices = [
    ...(bodyResult.truncated ? [MODEL_TRUNCATION_NOTICE] : []),
    ...(phuxTruncated ? [PHUX_TRUNCATION_NOTICE] : []),
  ];
  return {
    text: [header, ...(bodyResult.content.length === 0 ? [] : [bodyResult.content]), ...notices].join("\n"),
    truncated: bodyResult.truncated,
  };
}

export function toolResult(text: string, details: PhuxToolDetails): AgentToolResult<PhuxToolDetails> {
  return { content: [{ type: "text", text }], details };
}

export function sanitizeRenderText(text: string): string {
  return text
    .replace(/\x1B\][^\x07]*(?:\x07|\x1B\\)/g, "")
    .replace(/(?:\x1B\[|\x9B)[0-?]*[ -/]*[@-~]/g, "")
    .replace(/\x1B[@-_]/g, "")
    .replace(/[\x00-\x1F\x7F-\x9F]/g, " ");
}

export function callText(theme: Theme, text: string): Text {
  const safe = sanitizeRenderText(`phux ${text}`);
  return new Text(theme.fg("toolTitle", theme.bold(safe)), 0, 0);
}

export function renderSummary(
  result: AgentToolResult<PhuxToolDetails>,
  _options: unknown,
  theme: Theme,
): Text {
  const details = result.details;
  if (details === undefined) return new Text(theme.fg("error", "phux tool failed"), 0, 0);
  const suffix = details.modelOutputTruncated ? " (model output truncated)" : "";
  const safe = sanitizeRenderText(`${details.summary}${suffix}`);
  return new Text(theme.fg(details.exitCode === undefined || details.exitCode === 0 ? "success" : "warning", safe), 0, 0);
}

/** Bound inventory rows from the start, so the header and the first rows survive a large fleet. */
export function headBoundedResult(header: string, body: string): { readonly text: string; readonly truncated: boolean } {
  const notice = `[Pi adapter truncated the list to its first ${String(MAX_MODEL_LINES)} lines within ${String(MAX_MODEL_BYTES)} bytes]`;
  const reserved = Buffer.byteLength(header) + Buffer.byteLength(notice) + 2;
  const bodyResult = truncateHead(body, { maxBytes: Math.max(0, MAX_MODEL_BYTES - reserved), maxLines: Math.max(0, MAX_MODEL_LINES - 2) });
  return {
    text: [header, ...(bodyResult.content.length === 0 ? [] : [bodyResult.content]), ...(bodyResult.truncated ? [notice] : [])].join("\n"),
    truncated: bodyResult.truncated,
  };
}
