import assert from "node:assert/strict";
import test from "node:test";

import { TranscriptGate } from "@phux/integration-runtime/transcript";

import { PiTranscript, messageEntries, toolOutput } from "../src/transcript.js";

const assistantMessage = (content: unknown[], extra: Record<string, unknown> = {}) => ({
  role: "assistant", content, timestamp: 42, ...extra,
});

function mapper(clock: { now: number }): PiTranscript {
  return new PiTranscript(new TranscriptGate(() => clock.now));
}

test("a user message becomes one final user entry", () => {
  assert.deepEqual(messageEntries({ role: "user", content: "fix it", timestamp: 7 }, true), [
    { id: "user-7", role: "user", text: "fix it", truncated: false, final: true },
  ]);
  assert.deepEqual(messageEntries({
    role: "user", timestamp: 7, content: [{ type: "text", text: "a" }, { type: "image", data: "x" }, { type: "text", text: "b" }],
  }, true)[0]?.text, "a\nb");
  assert.deepEqual(messageEntries({ role: "user", content: "  ", timestamp: 7 }, true), []);
  assert.deepEqual(messageEntries({ role: "toolResult", content: "x", timestamp: 7 }, true), []);
  assert.deepEqual(messageEntries({ role: "custom", content: "phux context", timestamp: 7 }, true), []);
});

test("an assistant message yields its thinking and reply, and an error-only reply a system entry", () => {
  const message = assistantMessage([
    { type: "thinking", thinking: "plan" },
    { type: "thinking", thinking: "secret", redacted: true },
    { type: "text", text: "Done." },
    { type: "toolCall", id: "c", name: "bash", arguments: {} },
  ]);
  assert.deepEqual(messageEntries(message, true), [
    { id: "thinking-42", role: "thinking", text: "plan", truncated: false, final: true },
    { id: "assistant-42", role: "assistant", text: "Done.", truncated: false, final: true },
  ]);
  assert.deepEqual(messageEntries(message, false), [
    { id: "assistant-42", role: "assistant", text: "Done.", truncated: false, final: false },
  ], "thinking is final only");
  assert.deepEqual(messageEntries(assistantMessage([], { errorMessage: "rate limited" }), true), [
    { id: "error-42", role: "system", text: "rate limited", truncated: false, final: true },
  ]);
  assert.deepEqual(messageEntries(assistantMessage([{ type: "toolCall", id: "c", name: "x", arguments: {} }]), true), []);
});

test("streaming partials are throttled, grow, and give way to the final", () => {
  const clock = { now: 0 };
  const transcript = mapper(clock);
  const partial = (text: string) => assistantMessage([{ type: "text", text }]);
  const sent = (records: ReturnType<PiTranscript["messageEnd"]>) =>
    records.map((data) => `${data.entry.id}:${data.entry.final ? "final" : "partial"}:${data.entry.text}`);

  assert.deepEqual(sent(transcript.messageUpdate(partial("He"), "text_delta")), ["assistant-42:partial:He"]);
  clock.now = 100;
  assert.deepEqual(sent(transcript.messageUpdate(partial("Hell"), "text_delta")), [], "under 250 ms");
  clock.now = 260;
  assert.deepEqual(sent(transcript.messageUpdate(partial("Hello"), "text_delta")), ["assistant-42:partial:Hello"]);
  clock.now = 600;
  assert.deepEqual(sent(transcript.messageUpdate(partial("Hello"), "toolcall_delta")), [], "only text events stream");
  const thinking = assistantMessage([{ type: "thinking", thinking: "hmm" }, { type: "text", text: "Hello!" }]);
  assert.deepEqual(sent(transcript.messageUpdate(thinking, "thinking_end")), ["thinking-42:final:hmm"]);
  assert.deepEqual(sent(transcript.messageEnd(thinking)), ["assistant-42:final:Hello!"],
    "the ended thinking is not resent");
  clock.now = 5_000;
  assert.deepEqual(sent(transcript.messageUpdate(partial("Hello!!"), "text_delta")), [],
    "a late partial never replaces its final");
});

test("a tool call is one id: running with its summary, then its result", () => {
  const transcript = mapper({ now: 0 });
  const [running] = transcript.toolStart({ toolCallId: "call-1", toolName: "bash", args: { command: "ls\n -la" } });
  assert.deepEqual(running, {
    provider: "pi",
    schema: "phux.transcript/v1",
    entry: {
      id: "call-1", role: "tool", text: "", truncated: false, final: false,
      tool: { name: "bash", call_id: "call-1", summary: "ls -la", status: "running", output: "" },
    },
  });
  const [ended] = transcript.toolEnd({
    toolCallId: "call-1", toolName: "bash", isError: true,
    result: { content: [{ type: "text", text: "\u001b[31mno such file\u001b[0m" }], details: { exit: 1 } },
  });
  assert.deepEqual(ended?.entry.tool, {
    name: "bash", call_id: "call-1", summary: "ls -la", status: "error", output: "no such file",
  });
  assert.equal(ended?.entry.final, true);
});

test("tool output reads text content and nothing else", () => {
  assert.equal(toolOutput({ content: [{ type: "image", data: "AAAA" }], details: { big: true } }), "");
  assert.equal(toolOutput("plain"), "plain");
  assert.equal(toolOutput(undefined), "");
  assert.equal(toolOutput({ ok: true }), '{"ok":true}');
});

test("a partial that had to be cut stops the stream until its final", () => {
  const clock = { now: 0 };
  const transcript = mapper(clock);
  const partial = (text: string) => assistantMessage([{ type: "text", text }]);
  const long = "x".repeat(20_000);
  const [first] = transcript.messageUpdate(partial(long), "text_delta");
  assert.equal(first?.entry.truncated, true);
  clock.now = 1_000;
  assert.deepEqual(transcript.messageUpdate(partial(`${long}more`), "text_delta"), [],
    "a grown partial would repeat the same cut text");
  const [final] = transcript.messageEnd(partial(`${long}more`));
  assert.equal(final?.entry.final, true);
  assert.equal(final?.entry.truncated, true);
});
