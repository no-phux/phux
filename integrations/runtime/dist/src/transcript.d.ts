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
export declare const TRANSCRIPT_SCHEMA = "phux.transcript/v1";
/** The codec's line ceiling, envelope included. */
export declare const MAX_RECORD_BYTES: number;
/** Bytes kept free for the server's envelope around `data`. */
export declare const ENVELOPE_RESERVE_BYTES = 256;
export declare const MAX_TRANSCRIPT_DATA_BYTES: number;
export declare const MAX_TOOL_SUMMARY_CHARS = 512;
export declare const MAX_TOOL_OUTPUT_BYTES: number;
export declare const MAX_LABEL_BYTES = 256;
/** Minimum spacing between two partials of one entry. */
export declare const PARTIAL_MIN_INTERVAL_MS = 250;
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
export declare function transcriptEnabled(value: string | undefined): boolean;
export declare function utf8Length(text: string): number;
/** The longest prefix of `text` within `maxBytes` UTF-8 bytes, never splitting a code point. */
export declare function keepHead(text: string, maxBytes: number): string;
/** The longest suffix of `text` within `maxBytes` UTF-8 bytes, never splitting a code point. */
export declare function keepTail(text: string, maxBytes: number): string;
/** Strip terminal escape sequences and controls other than newline and tab. */
export declare function cleanText(text: string): string;
/** `text` cleaned, whitespace collapsed to single spaces, cut to `maxChars` code points. */
export declare function oneLine(text: string, maxChars: number): string;
/** A one-line summary of tool arguments, at most MAX_TOOL_SUMMARY_CHARS. */
export declare function summarizeArgs(args: unknown): string;
/**
 * The bounded `provider_raw` data for one entry: escapes and controls
 * stripped, each field within its ceiling, and the serialized object within
 * MAX_TRANSCRIPT_DATA_BYTES. `text` is cut first (head kept, `truncated`
 * set), then the tool output (tail kept).
 */
export declare function transcriptData(provider: string, entry: TranscriptEntry): TranscriptData;
/**
 * Decides which entries are worth a record. A final is sent unless it
 * repeats what was last sent for its id. A partial is sent only when its id
 * has no final yet, its text grew, and at least `intervalMs` passed since the
 * previous partial of that id. Remembers at most `capacity` ids.
 */
export declare class TranscriptGate {
    private readonly now;
    private readonly intervalMs;
    private readonly capacity;
    private readonly sent;
    constructor(now?: () => number, intervalMs?: number, capacity?: number);
    admit(entry: TranscriptEntry): boolean;
    private remember;
}
