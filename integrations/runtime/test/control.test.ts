import assert from "node:assert/strict";
import test from "node:test";
import { PhuxCli } from "../src/adapter.js";
import { PhuxError } from "../src/errors.js";
import { boundedResult, createPhuxTools, MAX_MODEL_BYTES, MAX_MODEL_LINES } from "../src/tools.js";
import type { ProcessResult } from "../src/runner.js";

function completed(value: unknown, exitCode = 0): ProcessResult {
  return { termination: "completed", exitCode, stdout: JSON.stringify(value), stderr: "" };
}

test("prompt timeout preserves acknowledged delivery; unknown delivery is not retried", async () => {
  const receipt = { schema_version: 1, terminal: "@7", delivery: "acked", operation_id: "op", transition_observed: false };
  const timed = new PhuxCli({ runner: async () => completed(receipt, 124) });
  assert.equal((await timed.agentPrompt("@7", "work")).delivery, "acked");
  let calls = 0;
  const unknown = new PhuxCli({ runner: async () => {
    calls++;
    return { termination: "completed", exitCode: 1, stdout: "", stderr: JSON.stringify({ error: { code: "delivery_unknown", message: "uncertain" }, remedy: "DO NOT RESEND" }) };
  } });
  await assert.rejects(unknown.agentPrompt("@7", "work"), (error: unknown) => {
    assert.ok(error instanceof PhuxError);
    assert.deepEqual(error.cliError?.error, { code: "delivery_unknown", message: "uncertain" });
    return true;
  });
  assert.equal(calls, 1);
});

test("resource gone is an observable outcome, but contradictory exit status fails closed", async () => {
  const gone = { schema_version: 1, resource: "@7", outcome: "gone", cursor: "cursor", evidence_lost: true };
  const cli = new PhuxCli({ runner: async () => completed(gone, 1) });
  assert.equal((await cli.resourceWait("@7")).outcome, "gone");
  const broken = new PhuxCli({ runner: async () => completed(gone, 0) });
  await assert.rejects(broken.resourceWait("@7"), (error: unknown) => error instanceof PhuxError && error.code === "invalid_response");
});

test("resource cursor fallback warnings remain visible to the model", async () => {
  const outcome = { schema_version: 1, resource: "@7", outcome: "timed_out", cursor: "new:1", evidence_lost: false };
  const cli = new PhuxCli({ runner: async () => ({
    ...completed(outcome, 124),
    stderr: "the --after cursor belongs to another server run; answered from current state instead",
  }) });
  const tools = createPhuxTools({ cli, getSelectedTarget: () => "@7", selectTarget: () => {} });
  const result = await tools.phux_resource_wait!.execute({ after: "old:1" }, {
    sessionID: "test", messageID: "message", id: "call", agent: "test",
  });
  assert.match(result.content, /cursor belongs to another server run/);
  assert.match(result.content, /timed_out/);
});

test("native tools reject ambiguous and self targets before any input can be sent", async () => {
  let calls = 0;
  const cli = new PhuxCli({ runner: async () => { calls++; return completed({}); } });
  const tools = createPhuxTools({ cli, parentTarget: "@7", getSelectedTarget: () => "@7", selectTarget: () => {} });
  const context = { sessionID: "test", messageID: "message", id: "call", agent: "test" };
  for (const target of [undefined, "@007", "work", ".", "#workers", "@7 --help"]) {
    await assert.rejects(tools.phux_paste!.execute({ target, text: "input" }, context));
  }
  await assert.rejects(tools.phux_run!.execute({ target: "@8", command: "work", timeout_seconds: 0 }, context));
  assert.equal(calls, 0);
});

test("bounded output preserves UTF-8 and result facts under byte and line floods", () => {
  const result = boundedResult("run exit=7", "old\n".repeat(1000) + "界".repeat(10_000), true);
  assert.ok(Buffer.byteLength(result.text, "utf8") <= MAX_MODEL_BYTES);
  assert.ok(result.text.split("\n").length <= MAX_MODEL_LINES);
  assert.ok(result.text.startsWith("run exit=7\n"));
  assert.ok(!result.text.includes("\ufffd"));
  assert.equal(result.truncated, true);
});
