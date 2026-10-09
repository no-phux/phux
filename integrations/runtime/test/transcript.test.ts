import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_RECORD_BYTES,
  MAX_TOOL_OUTPUT_BYTES,
  MAX_TOOL_SUMMARY_CHARS,
  MAX_TRANSCRIPT_DATA_BYTES,
  TranscriptGate,
  cleanText,
  keepHead,
  keepTail,
  summarizeArgs,
  transcriptData,
  transcriptEnabled,
  utf8Length,
  type TranscriptEntry,
} from "../src/transcript.js";

const assistant = (text: string, final = true, id = "assistant-1"): TranscriptEntry => ({
  id, role: "assistant", text, truncated: false, final,
});

/** The retained record line with the widest server-stamped header. */
function retainedLine(data: unknown): string {
  return `{"seq":18446744073709551615,"ts_ms":18446744073709551615,"type":"provider_raw","data":${JSON.stringify(data)}}\n`;
}

test("data has the documented shape", () => {
  assert.deepEqual(transcriptData("pi", assistant("hello")), {
    provider: "pi",
    schema: "phux.transcript/v1",
    entry: { id: "assistant-1", role: "assistant", text: "hello", truncated: false, final: true },
  });
  const tool = transcriptData("pi", {
    id: "call-1", role: "tool", text: "", truncated: false, final: false,
    tool: { name: "bash", call_id: "call-1", summary: "ls\n  -la", status: "running", output: "" },
  });
  assert.deepEqual(tool.entry.tool, { name: "bash", call_id: "call-1", summary: "ls -la", status: "running", output: "" });
  const stray = transcriptData("pi", { ...assistant("x"), tool: tool.entry.tool! });
  assert.equal("tool" in stray.entry, false, "a tool field rides only a tool entry");
});

test("a huge text is cut on a code point boundary and the record still fits", () => {
  const text = "\"é€😀".repeat(20_000);
  const data = transcriptData("pi", assistant(text));
  assert.ok(utf8Length(retainedLine(data)) <= MAX_RECORD_BYTES);
  assert.ok(utf8Length(JSON.stringify(data)) <= MAX_TRANSCRIPT_DATA_BYTES);
  assert.equal(data.entry.truncated, true);
  assert.ok(text.startsWith(data.entry.text), "the head is kept");
  assert.ok(utf8Length(data.entry.text) > 8 * 1024, "the cut is not wasteful");
  assert.doesNotThrow(() => encodeURIComponent(data.entry.text), "no lone surrogate");
});

test("tool output keeps its tail and the summary its cap", () => {
  const data = transcriptData("pi", {
    id: "c", role: "tool", text: "", truncated: false, final: true,
    tool: { name: "bash", call_id: "c", summary: "s".repeat(2_000), status: "ok", output: `${"x".repeat(10_000)}TAIL` },
  });
  assert.equal(utf8Length(data.entry.tool!.output), MAX_TOOL_OUTPUT_BYTES);
  assert.ok(data.entry.tool!.output.endsWith("TAIL"));
  assert.equal(Array.from(data.entry.tool!.summary).length, MAX_TOOL_SUMMARY_CHARS);
  assert.equal(data.entry.truncated, false);
});

test("escape sequences and controls are stripped", () => {
  assert.equal(
    cleanText("\u001b[1;31mred\u001b[0m \u001b]0;title\u0007ok\r\n\ttab\u0000\u007f\u001b]8;;x\u001b\\!"),
    "red ok\n\ttab!",
  );
});

test("head and tail cuts never split a code point", () => {
  assert.equal(keepHead("a€b", 2), "a");
  assert.equal(keepTail("a€b", 3), "b");
  assert.equal(keepTail("abc", 3), "abc");
  assert.equal(keepTail("x😀", 4), "😀");
  assert.equal(keepTail("x😀", 3), "");
  assert.equal(keepHead("😀", 3), "");
});

test("argument summaries name what the call acts on", () => {
  assert.equal(summarizeArgs({ command: "cargo test\n -p x", timeout: 5 }), "cargo test -p x");
  assert.equal(summarizeArgs({ path: "src/lib.rs", offset: 1 }), "src/lib.rs");
  assert.equal(summarizeArgs({ all: true }), '{"all":true}');
  assert.equal(summarizeArgs(undefined), "");
  assert.equal(summarizeArgs("raw"), "raw");
});

test("the opt-out is exactly 0", () => {
  assert.equal(transcriptEnabled(undefined), true);
  assert.equal(transcriptEnabled("1"), true);
  assert.equal(transcriptEnabled(""), true);
  assert.equal(transcriptEnabled("0"), false);
  assert.equal(transcriptEnabled(" 0 "), true, "exactly 0, as the shell hooks compare it");
});

test("partials are throttled, must grow, and never follow their final", () => {
  let now = 1_000;
  const gate = new TranscriptGate(() => now);
  assert.equal(gate.admit(assistant("a", false)), true, "the first partial goes");
  now += 100;
  assert.equal(gate.admit(assistant("ab", false)), false, "too soon");
  now += 200;
  assert.equal(gate.admit(assistant("ab", false)), true, "250 ms later and grown");
  now += 300;
  assert.equal(gate.admit(assistant("ab", false)), false, "not grown");
  assert.equal(gate.admit(assistant("abc", true)), true, "a final always goes");
  assert.equal(gate.admit(assistant("abc", true)), false, "a repeated final does not");
  now += 1_000;
  assert.equal(gate.admit(assistant("abcd", false)), false, "a late partial never replaces its final");
  assert.equal(gate.admit(assistant("abcd", true)), true, "a changed final does");
  assert.equal(gate.admit(assistant("x", false, "assistant-2")), true, "ids are independent");
});

test("the gate remembers a bounded number of ids", () => {
  const gate = new TranscriptGate(() => 0, 250, 2);
  assert.equal(gate.admit(assistant("a", true, "1")), true);
  assert.equal(gate.admit(assistant("a", true, "2")), true);
  assert.equal(gate.admit(assistant("a", true, "3")), true);
  assert.equal(gate.admit(assistant("a", true, "1")), true, "the oldest id was forgotten");
  assert.equal(gate.admit(assistant("a", true, "3")), false);
});
