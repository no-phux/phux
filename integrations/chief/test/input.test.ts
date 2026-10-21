import assert from "node:assert/strict";
import test from "node:test";

import { parseInput } from "../src/envelope.js";
import { LineInput } from "../src/line-input.js";

const submits = (input: LineInput, ...chunks: string[]) =>
  chunks.flatMap((chunk) => input.feed(chunk)).filter((event) => event.type === "submit").map((event) => (event as { text: string }).text);

test("a line longer than MAX_CANON assembles across chunks and submits on Enter", () => {
  const input = new LineInput();
  const long = "x".repeat(5000);
  assert.deepEqual(submits(input, long.slice(0, 1000), long.slice(1000), "\r"), [long]);
});

test("a bracketed paste keeps its newlines and Enter submits it", () => {
  const input = new LineInput();
  assert.deepEqual(submits(input, "\u001b[200~line one\nline", " two\u001b[2", "01~", "\r"), ["line one\nline two"]);
});

test("backspace edits, CRLF is one Enter, blank lines submit nothing, Ctrl-C interrupts", () => {
  const input = new LineInput();
  assert.deepEqual(submits(input, "abd\u007fc\r\n", "\r", "   \r"), ["abc"]);
  assert.ok(input.feed("half\u0003").some((event) => event.type === "interrupt"));
  assert.deepEqual(submits(input, "\r"), [], "Ctrl-C cleared the buffer");
});

test("arrow keys and other escapes are ignored, also when split", () => {
  const input = new LineInput();
  assert.deepEqual(submits(input, "a\u001b[", "A\u001bOBb\r"), ["ab"]);
});

test("plain text and slash commands are an unlocked human at the host", () => {
  assert.deepEqual(parseInput("  check CI  "), { ok: true, command: { type: "message", id: undefined, text: "check CI", locked: false } });
  assert.deepEqual(parseInput("/model anthropic/claude-sonnet-5 high"), { ok: true, command: { type: "model", id: undefined, model: "anthropic/claude-sonnet-5", thinking: "high" } });
  assert.deepEqual(parseInput("/code L1 https://x/cb?code=abc"), { ok: true, command: { type: "login_answer", id: undefined, login: "L1", value: "https://x/cb?code=abc" } });
  assert.equal(parseInput("   "), null);
  assert.equal(parseInput("/nope")?.ok, false);
});

test("envelopes carry ids, multi-line text, and the lock flag", () => {
  const parsed = parseInput(JSON.stringify({ chief: 1, id: "m-1", type: "message", text: "a\nb", locked: true }));
  assert.deepEqual(parsed, { ok: true, command: { type: "message", id: "m-1", text: "a\nb", locked: true } });
  assert.deepEqual(parseInput("{\"chief\":1,\"id\":\"u\",\"type\":\"unlock\"}"), { ok: true, command: { type: "unlock", id: "u" } });
  assert.deepEqual(parseInput("{\"chief\":1,\"type\":\"login\",\"provider\":\"openai-codex\",\"method\":\"oauth\"}"),
    { ok: true, command: { type: "login", id: undefined, provider: "openai-codex", method: "oauth" } });
});

test("bad envelopes are refused with the id echoed when it is valid", () => {
  assert.deepEqual(parseInput("{\"chief\":2,\"id\":\"x\",\"type\":\"status\"}"), { ok: false, id: "x", error: "unsupported envelope version 2; this chief speaks 1" });
  assert.equal(parseInput("{\"chief\":1,\"id\":\"has space\",\"type\":\"status\"}")?.ok, false);
  assert.deepEqual(parseInput("{\"chief\":1,\"id\":\"y\",\"type\":\"message\",\"text\":\"\"}"), { ok: false, id: "y", error: "text must be a non-empty string" });
  assert.deepEqual(parseInput("{\"chief\":1,\"id\":\"z\",\"type\":\"message\",\"text\":\"t\",\"locked\":\"yes\"}"), { ok: false, id: "z", error: "locked must be a boolean" });
  assert.equal(parseInput("{\"chief\": broken")?.ok, false);
});
