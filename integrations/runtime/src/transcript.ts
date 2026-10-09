/**
 * The `phux.transcript/v1` payload convention (ADR-0156): one conversation
 * entry per `provider_raw` AgentSession record, as
 * `data = { provider, schema, entry }`. An entry with a repeated `id`
 * replaces the earlier one; `final: false` marks a streaming partial.
 *
 * The bounds mirror `phux_client_core::session::agent_transcript`: the
 * serialized `data` stays within MAX_TRANSCRIPT_DATA_BYTES so the retained
 * record fits the 16 KiB codec line.
 */

export const TRANSCRIPT_SCHEMA = "phux.transcript/v1";
/** The codec's line ceiling, envelope included. */
export const MAX_RECORD_BYTES = 16 * 1024;
/** Bytes kept free for the server's envelope around `data`. */
export const ENVELOPE_RESERVE_BYTES = 256;
export const MAX_TRANSCRIPT_DATA_BYTES = MAX_RECORD_BYTES - ENVELOPE_RESERVE_BYTES;
export const MAX_TOOL_SUMMARY_CHARS = 512;
export const MAX_TOOL_OUTPUT_BYTES = 4 * 1024;
export const MAX_LABEL_BYTES = 256;
/** Minimum spacing between two partials of one entry. */
export const PARTIAL_MIN_INTERVAL_MS = 250;

export type TranscriptRole = "user" | "assistant" | "thinking" | "tool" | "system";
export type ToolStatus = "running" | "ok" | "error";

export type TranscriptTool = {
  readonly name: string;
  readonly call_id: string;
  readonly summary: string;
  readonly status: ToolStatus;
  readonly output: string;
};

export type TranscriptEntry = {
  readonly id: string;
  readonly role: TranscriptRole;
  readonly text: string;
  readonly truncated: boolean;
  readonly final: boolean;
  readonly tool?: TranscriptTool;
};

export type TranscriptData = {
  readonly provider: string;
  readonly schema: typeof TRANSCRIPT_SCHEMA;
  readonly entry: TranscriptEntry;
};

/** Transcript records are on unless the variable is exactly `0`. */
export function transcriptEnabled(value: string | undefined): boolean {
  return value !== "0";
}

const encoder = new TextEncoder();

export function utf8Length(text: string): number {
  return Buffer.byteLength(text, "utf8");
}

/** The longest prefix of `text` within `maxBytes` UTF-8 bytes, never splitting a code point. */
export function keepHead(text: string, maxBytes: number): string {
  if (utf8Length(text) <= maxBytes) return text;
  const { read } = encoder.encodeInto(text, new Uint8Array(Math.max(0, maxBytes)));
  return text.slice(0, read);
}

/** The longest suffix of `text` within `maxBytes` UTF-8 bytes, never splitting a code point. */
export function keepTail(text: string, maxBytes: number): string {
  let bytes = utf8Length(text);
  if (bytes <= maxBytes) return text;
  let tail = text.slice(-Math.max(0, maxBytes));
  bytes = utf8Length(tail);
  while (bytes > maxBytes) {
    tail = tail.slice(Math.max(1, Math.ceil((bytes - maxBytes) / 3)));
    bytes = utf8Length(tail);
  }
  // A cut between a surrogate pair leaves a lone low surrogate at the front.
  const first = tail.charCodeAt(0);
  return first >= 0xdc00 && first <= 0xdfff ? tail.slice(1) : tail;
}

// CSI (parameters, intermediates, final byte), OSC (to BEL or ST), and
// two-character escapes; then every control except newline and tab.
const ESCAPES = /\u001b(?:\[[0-?]*[ -/]*[@-~]|\][^\u0007\u001b]*(?:\u0007|\u001b\\)?|[\s\S]?)/g;
const CONTROLS = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g;

/** Strip terminal escape sequences and controls other than newline and tab. */
export function cleanText(text: string): string {
  return text.replace(ESCAPES, "").replace(CONTROLS, "");
}

/** `text` cleaned, whitespace collapsed to single spaces, cut to `maxChars` code points. */
export function oneLine(text: string, maxChars: number): string {
  const collapsed = cleanText(text).split(/\s+/u).filter((word) => word.length > 0).join(" ");
  return Array.from(collapsed).slice(0, maxChars).join("");
}

function label(text: string): string {
  return keepHead(oneLine(text, MAX_LABEL_BYTES), MAX_LABEL_BYTES);
}

/** Keys of a tool's arguments that name what it acts on, most telling first. */
const SUMMARY_KEYS = ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"];

/** A one-line summary of tool arguments, at most MAX_TOOL_SUMMARY_CHARS. */
export function summarizeArgs(args: unknown): string {
  let summary = "";
  if (typeof args === "string") {
    summary = args;
  } else if (args !== null && typeof args === "object" && !Array.isArray(args)) {
    const record = args as Record<string, unknown>;
    const named = SUMMARY_KEYS
      .map((key) => record[key])
      .find((value): value is string => typeof value === "string" && value.trim().length > 0);
    summary = named ?? safeJson(args);
  } else if (args !== undefined && args !== null) {
    summary = safeJson(args);
  }
  return oneLine(summary, MAX_TOOL_SUMMARY_CHARS);
}

function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value) ?? "";
  } catch {
    return "";
  }
}

/**
 * The bounded `provider_raw` data for one entry: escapes and controls
 * stripped, each field within its ceiling, and the serialized object within
 * MAX_TRANSCRIPT_DATA_BYTES. `text` is cut first (head kept, `truncated`
 * set), then the tool output (tail kept).
 */
export function transcriptData(provider: string, entry: TranscriptEntry): TranscriptData {
  const cleaned = cleanText(entry.text);
  let text = keepHead(cleaned, MAX_TRANSCRIPT_DATA_BYTES);
  let truncated = entry.truncated || text.length < cleaned.length;
  let tool = entry.role === "tool" && entry.tool !== undefined
    ? {
      name: label(entry.tool.name),
      call_id: label(entry.tool.call_id),
      summary: oneLine(entry.tool.summary, MAX_TOOL_SUMMARY_CHARS),
      status: entry.tool.status,
      output: keepTail(cleanText(entry.tool.output), MAX_TOOL_OUTPUT_BYTES),
    }
    : undefined;
  const build = (): TranscriptData => ({
    provider: label(provider),
    schema: TRANSCRIPT_SCHEMA,
    entry: {
      id: label(entry.id),
      role: entry.role,
      text,
      truncated,
      final: entry.final,
      ...(tool === undefined ? {} : { tool }),
    },
  });
  for (;;) {
    const data = build();
    const over = utf8Length(JSON.stringify(data)) - MAX_TRANSCRIPT_DATA_BYTES;
    if (over <= 0) return data;
    if (text.length > 0) {
      text = keepHead(text, utf8Length(text) - over);
      truncated = true;
    } else if (tool !== undefined && tool.output.length > 0) {
      tool = { ...tool, output: keepTail(tool.output, utf8Length(tool.output) - over) };
    } else {
      // Every remaining field is bounded far below the ceiling.
      return data;
    }
  }
}

interface Emitted {
  readonly text: string;
  readonly final: boolean;
  readonly status: ToolStatus | undefined;
  readonly at: number;
}

/**
 * Decides which entries are worth a record. A final is sent unless it
 * repeats what was last sent for its id. A partial is sent only when its id
 * has no final yet, its text grew, and at least `intervalMs` passed since the
 * previous partial of that id. Remembers at most `capacity` ids.
 */
export class TranscriptGate {
  private readonly sent = new Map<string, Emitted>();

  constructor(
    private readonly now: () => number = Date.now,
    private readonly intervalMs: number = PARTIAL_MIN_INTERVAL_MS,
    private readonly capacity: number = 256,
  ) {}

  admit(entry: TranscriptEntry): boolean {
    const previous = this.sent.get(entry.id);
    const at = this.now();
    if (entry.final) {
      if (previous?.final === true && previous.text === entry.text && previous.status === entry.tool?.status) {
        return false;
      }
    } else if (previous !== undefined) {
      if (previous.final) return false;
      if (entry.text.length <= previous.text.length) return false;
      if (at - previous.at < this.intervalMs) return false;
    }
    this.remember(entry, at);
    return true;
  }

  private remember(entry: TranscriptEntry, at: number): void {
    this.sent.delete(entry.id);
    this.sent.set(entry.id, { text: entry.text, final: entry.final, status: entry.tool?.status, at });
    if (this.sent.size > this.capacity) {
      const oldest = this.sent.keys().next().value;
      if (oldest !== undefined) this.sent.delete(oldest);
    }
  }
}
