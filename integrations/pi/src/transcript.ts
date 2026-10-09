import {
  TranscriptGate,
  summarizeArgs,
  transcriptData,
  type TranscriptData,
  type TranscriptEntry,
} from "@phux/integration-runtime/transcript";

/** The provider word on every Pi transcript record. */
export const PI_TRANSCRIPT_PROVIDER = "pi";

interface ContentBlock {
  readonly type?: unknown;
  readonly text?: unknown;
  readonly thinking?: unknown;
  readonly redacted?: unknown;
}

/** The fields of a Pi agent message this mapping reads. */
export interface PiMessageLike {
  readonly role?: unknown;
  readonly content?: unknown;
  readonly timestamp?: unknown;
  readonly errorMessage?: unknown;
}

export interface PiToolStartLike {
  readonly toolCallId: string;
  readonly toolName: string;
  readonly args?: unknown;
}

export interface PiToolEndLike {
  readonly toolCallId: string;
  readonly toolName: string;
  readonly result?: unknown;
  readonly isError: boolean;
}

function blocks(content: unknown): readonly ContentBlock[] {
  if (typeof content === "string") return [{ type: "text", text: content }];
  return Array.isArray(content) ? content.filter((block): block is ContentBlock =>
    block !== null && typeof block === "object") : [];
}

/** Joined text blocks of a message (or tool result) content. */
export function contentText(content: unknown): string {
  return blocks(content)
    .filter((block) => block.type === "text" && typeof block.text === "string")
    .map((block) => block.text as string)
    .join("\n");
}

/** Joined visible thinking blocks of an assistant message. */
export function thinkingText(content: unknown): string {
  return blocks(content)
    .filter((block) => block.type === "thinking" && block.redacted !== true && typeof block.thinking === "string")
    .map((block) => block.thinking as string)
    .filter((text) => text.trim().length > 0)
    .join("\n\n");
}

function stamp(message: PiMessageLike): string {
  return typeof message.timestamp === "number" && Number.isFinite(message.timestamp)
    ? String(message.timestamp)
    : "0";
}

function textEntry(id: string, role: TranscriptEntry["role"], text: string, final: boolean): TranscriptEntry {
  return { id, role, text, truncated: false, final };
}

/**
 * The entries one Pi message contributes, in order: for an assistant
 * message its visible thinking, then its reply text (or, with neither and a
 * provider error, a system entry); for a user message its text. Ids derive
 * from the role and the message timestamp, which Pi fixes at message start,
 * so a partial and its final share an id.
 */
export function messageEntries(message: PiMessageLike, final: boolean): TranscriptEntry[] {
  const at = stamp(message);
  if (message.role === "user") {
    const text = contentText(message.content);
    return final && text.trim().length > 0 ? [textEntry(`user-${at}`, "user", text, true)] : [];
  }
  if (message.role !== "assistant") return [];
  const entries: TranscriptEntry[] = [];
  const thinking = thinkingText(message.content);
  if (final && thinking.length > 0) entries.push(textEntry(`thinking-${at}`, "thinking", thinking, true));
  const text = contentText(message.content);
  if (text.trim().length > 0) {
    entries.push(textEntry(`assistant-${at}`, "assistant", text, final));
  } else if (final && thinking.length === 0 && typeof message.errorMessage === "string" &&
    message.errorMessage.trim().length > 0) {
    entries.push(textEntry(`error-${at}`, "system", message.errorMessage, true));
  }
  return entries;
}

/** Thinking for the current message, final once its block ended. */
export function thinkingEntry(message: PiMessageLike): TranscriptEntry | null {
  const thinking = thinkingText(message.content);
  return thinking.length > 0 ? textEntry(`thinking-${stamp(message)}`, "thinking", thinking, true) : null;
}

/** Turns Pi events into bounded, gated `provider_raw` transcript data. */
export class PiTranscript {
  private readonly gate: TranscriptGate;
  private readonly summaries = new Map<string, string>();
  private readonly saturated = new Set<string>();

  constructor(gate: TranscriptGate = new TranscriptGate()) {
    this.gate = gate;
  }

  /** A streaming assistant update: a throttled partial, or thinking once its block ends. */
  messageUpdate(message: PiMessageLike, kind: string | undefined): TranscriptData[] {
    if (message.role !== "assistant") return [];
    const entries: TranscriptEntry[] = [];
    if (kind === "thinking_end") {
      const thinking = thinkingEntry(message);
      if (thinking !== null) entries.push(thinking);
    }
    if (kind === "text_delta" || kind === "text_end") entries.push(...messageEntries(message, false));
    return this.admit(entries);
  }

  messageEnd(message: PiMessageLike): TranscriptData[] {
    return this.admit(messageEntries(message, true));
  }

  toolStart(event: PiToolStartLike): TranscriptData[] {
    const summary = summarizeArgs(event.args);
    this.summaries.set(event.toolCallId, summary);
    if (this.summaries.size > 256) {
      const oldest = this.summaries.keys().next().value;
      if (oldest !== undefined) this.summaries.delete(oldest);
    }
    return this.admit([{
      id: event.toolCallId, role: "tool", text: "", truncated: false, final: false,
      tool: { name: event.toolName, call_id: event.toolCallId, summary, status: "running", output: "" },
    }]);
  }

  toolEnd(event: PiToolEndLike): TranscriptData[] {
    const summary = this.summaries.get(event.toolCallId) ?? "";
    this.summaries.delete(event.toolCallId);
    return this.admit([{
      id: event.toolCallId, role: "tool", text: "", truncated: false, final: true,
      tool: {
        name: event.toolName,
        call_id: event.toolCallId,
        summary,
        status: event.isError ? "error" : "ok",
        output: toolOutput(event.result),
      },
    }]);
  }

  /**
   * Gate on the raw entry first, so a streamed token costs a length and clock
   * check rather than a bounded serialization. Once a partial had to be cut,
   * later partials of that id would repeat it, so only its final follows.
   */
  private admit(entries: readonly TranscriptEntry[]): TranscriptData[] {
    const admitted: TranscriptData[] = [];
    for (const entry of entries) {
      if (!entry.final && this.saturated.has(entry.id)) continue;
      if (!this.gate.admit(entry)) continue;
      const data = transcriptData(PI_TRANSCRIPT_PROVIDER, entry);
      if (!entry.final && data.entry.truncated) this.remember(this.saturated, entry.id);
      if (entry.final) this.saturated.delete(entry.id);
      admitted.push(data);
    }
    return admitted;
  }

  private remember(ids: Set<string>, id: string): void {
    ids.add(id);
    if (ids.size > 256) {
      const oldest = ids.values().next().value;
      if (oldest !== undefined) ids.delete(oldest);
    }
  }
}

/** The readable text of a Pi tool result: its text blocks when it has content, else its JSON form. */
export function toolOutput(result: unknown): string {
  if (typeof result === "string") return result;
  if (result === null || typeof result !== "object") return result === undefined ? "" : String(result);
  const content = (result as { content?: unknown }).content;
  if (Array.isArray(content)) return contentText(content);
  try {
    return JSON.stringify(result) ?? "";
  } catch {
    return "";
  }
}
