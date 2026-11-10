import assert from "node:assert/strict";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import test from "node:test";

import { PhuxCli } from "../src/adapter.js";
import type { ProcessResult, RunRequest } from "../src/runner.js";
import { MAX_INLINE_IMAGE_BYTES } from "../src/resource-tools.js";
import { createPhuxTools, MAX_MODEL_BYTES } from "../src/tools.js";

const CONTEXT = { sessionID: "test", messageID: "message", id: "call", agent: "test" };
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==", "base64");
const TREE = { role: "AXWindow", title: "hello", children: [{ id: "n1", role: "AXButton", title: "OK" }] };

function completed(value: unknown, exitCode = 0, stderr = ""): ProcessResult {
  return { termination: "completed", exitCode, stdout: value === undefined ? "" : JSON.stringify(value), stderr };
}

function workload(message: string): ProcessResult {
  return completed(undefined, 1, JSON.stringify({ schema_version: 1, error: { code: "workload", message }, remedy: "", exit_code: 1 }));
}

function harness(respond: (request: RunRequest) => ProcessResult) {
  const requests: RunRequest[] = [];
  const cli = new PhuxCli({ socket: "/tmp/private.sock", runner: async (request) => { requests.push(request); return respond(request); } });
  const tools = createPhuxTools({ cli, parentTarget: "@1", getSelectedTarget: () => "@1", selectTarget: () => {} });
  return { requests, tools };
}

function outPath(request: RunRequest): string {
  const flag = request.args.find((arg) => arg.startsWith("--out="));
  assert.ok(flag, "surface snapshots must never put image bytes on stdout");
  return flag.slice("--out=".length);
}

test("document tools register with strict schemas that require an exact document target", () => {
  const { tools } = harness(() => completed({}));
  for (const name of ["phux_documents", "phux_tree_snapshot", "phux_tree_wait", "phux_surface_snapshot"]) {
    const input = tools[name]?.input as { additionalProperties?: boolean; required?: string[] } | undefined;
    assert.equal(input?.additionalProperties, false, name);
    if (name !== "phux_documents") assert.ok(input?.required?.includes("target"), name);
  }
  assert.deepEqual((tools.phux_tree_wait!.input as { required: string[] }).required, ["target", "until"]);
  assert.match(tools.phux_list!.description, /accessibility trees, pixel surfaces, typed documents/);
});

test("phux_list and phux_documents read every kind from one ls call", async () => {
  const fixture = JSON.parse(readFileSync(new URL("../../test/fixtures/session-list-v4.json", import.meta.url), "utf8"));
  const { requests, tools } = harness(() => completed(fixture));
  const listed = await tools.phux_list!.execute({}, CONTEXT);
  const result = listed.metadata.result as { terminals: string[]; resources: { kind: string }[] };
  assert.deepEqual(result.terminals, ["@1"]);
  assert.deepEqual(result.resources.map((row) => row.kind), ["terminal", "accessibility_tree", "pixel_surface", "document"]);
  const documents = await tools.phux_documents!.execute({}, CONTEXT);
  assert.deepEqual((documents.metadata.result as { documents: { id: string }[] }).documents.map((entry) => entry.id), ["@2", "@3", "@4"]);
  assert.match(documents.content, /"control": "page"/);
  assert.deepEqual(requests.map((request) => request.args), [["ls", "--json", "--socket", "/tmp/private.sock"], ["ls", "--json", "--socket", "/tmp/private.sock"]]);
});

test("phux_tree_snapshot invokes the CLI verb and returns the document", async () => {
  const { requests, tools } = harness(() => completed({ schema_version: 1, seq: 4, document: TREE }));
  const result = await tools.phux_tree_snapshot!.execute({ target: "@2" }, CONTEXT);
  assert.deepEqual(requests[0]?.args, ["agent", "tree", "snapshot", "--json", "--socket", "/tmp/private.sock", "--", "@2"]);
  assert.equal(requests[0]?.timeoutMs, 10_000);
  assert.equal(result.content, `tree_snapshot target=@2 seq=4\n${JSON.stringify(TREE)}`);
  assert.equal(result.metadata.seq, 4);
  for (const target of [undefined, "work", "@2 --help", "#trees"]) {
    await assert.rejects(tools.phux_tree_snapshot!.execute({ target }, CONTEXT));
  }
  assert.equal(requests.length, 1);
});

test("an oversized tree keeps its head in the model and its whole JSON on disk", async () => {
  const big = { role: "AXWindow", children: Array.from({ length: 2000 }, (_, index) => ({ id: `n${index}`, title: "界".repeat(8) })) };
  const { tools } = harness(() => completed({ schema_version: 1, seq: 9, document: big }));
  const result = await tools.phux_tree_snapshot!.execute({ target: "@2" }, CONTEXT);
  assert.ok(Buffer.byteLength(result.content, "utf8") <= MAX_MODEL_BYTES);
  assert.ok(result.content.startsWith('tree_snapshot target=@2 seq=9\n{"role":"AXWindow"'));
  assert.ok(!result.content.includes("\ufffd"));
  const path = result.metadata.documentPath as string;
  assert.deepEqual(JSON.parse(readFileSync(path, "utf8")), big);
  assert.equal(result.metadata.modelOutputTruncated, true);
  rmSync(path.replace(/\/tree\.json$/, ""), { recursive: true });
});

test("phux_tree_wait maps the CLI's match, timeout and closed tree to outcomes", async () => {
  const responses = [
    completed({ schema_version: 1, seq: 2, document: TREE }),
    workload("no document matched within 5s"),
    workload("the tree ended (Closed) before a document matched"),
    workload("the tree ended (Disconnected) before a document matched"),
  ];
  const { requests, tools } = harness(() => responses.shift()!);
  const matched = await tools.phux_tree_wait!.execute({ target: "@2", until: "-hello", timeout_seconds: 5 }, CONTEXT);
  assert.deepEqual(requests[0]?.args, ["agent", "tree", "follow", "--json", "--until=-hello", "--timeout", "5", "--socket", "/tmp/private.sock", "--", "@2"]);
  assert.equal(requests[0]?.timeoutMs, 10_000);
  assert.equal(matched.metadata.outcome, "matched");
  assert.match(matched.content, /outcome=matched\n.*"hello"/);
  const timedOut = await tools.phux_tree_wait!.execute({ target: "@2", until: "x", timeout_seconds: 5 }, CONTEXT);
  assert.deepEqual(timedOut.metadata.result, { schema_version: 1, outcome: "timed_out" });
  assert.match(timedOut.content, /^tree_wait target=@2 outcome=timed_out\n/);
  const closed = await tools.phux_tree_wait!.execute({ target: "@2", until: "x" }, CONTEXT);
  assert.deepEqual(closed.metadata.result, { schema_version: 1, outcome: "closed" });
  assert.equal(closed.metadata.outcome, "closed");
  assert.ok(requests[2]?.args.includes("30"), "default deadline is 30 seconds");
  await assert.rejects(tools.phux_tree_wait!.execute({ target: "@2", until: "x" }, CONTEXT), /Disconnected/);
  await assert.rejects(tools.phux_tree_wait!.execute({ target: "@2", until: "x", timeout_seconds: 3601 }, CONTEXT), /3600/);
  assert.equal(requests.length, 4);
});

test("phux_tree_wait outcomes track the CLI's error messages", () => {
  const source = readFileSync(new URL("../../../../crates/phux/src/commands/agent/tree.rs", import.meta.url), "utf8");
  assert.match(source, /format!\("no document matched within \{secs\}s"\)/);
  assert.match(source, /format!\("the tree ended \(\{end:\?\}\) before a document matched"\)/);
  assert.match(source, /json_err::codes::WORKLOAD,\s+format!\("no document matched/);
});

test("phux_surface_snapshot writes the frame to a private file and returns it as an image", async () => {
  let out = "";
  const { requests, tools } = harness((request) => {
    out = outPath(request);
    writeFileSync(out, PNG);
    return completed({ schema_version: 1, seq: 3, bytes: PNG.length, format: "png", width: 1, height: 1, content_type: null, out });
  });
  const result = await tools.phux_surface_snapshot!.execute({ target: "@3" }, CONTEXT);
  assert.deepEqual(requests[0]?.args, ["agent", "surface", "snapshot", "--json", `--out=${out}`, "--socket", "/tmp/private.sock", "--", "@3"]);
  assert.deepEqual(result.images, [{ data: PNG.toString("base64"), mimeType: "image/png" }]);
  assert.match(result.content, /^surface_snapshot target=@3 seq=3 bytes=70 format=png 1x1\nframe attached as an image$/);
  assert.equal(existsSync(out), false, "an inlined frame leaves no file behind");
  assert.equal(JSON.stringify(result.metadata).includes(PNG.toString("base64")), false);
});

test("an empty or oversized surface frame is reported without inlining", async () => {
  const empty = harness(() => completed({ schema_version: 1, seq: 0, bytes: 0, format: null, width: null, height: null }));
  const none = await empty.tools.phux_surface_snapshot!.execute({ target: "@3" }, CONTEXT);
  assert.equal(none.images, undefined);
  assert.match(none.content, /nothing published yet/);
  assert.equal(existsSync(outPath(empty.requests[0]!)), false);

  const large = harness((request) => {
    writeFileSync(outPath(request), Buffer.alloc(16));
    return completed({ schema_version: 1, seq: 5, bytes: MAX_INLINE_IMAGE_BYTES + 1, format: "jpeg", width: 4000, height: 3000 });
  });
  const kept = await large.tools.phux_surface_snapshot!.execute({ target: "@3" }, CONTEXT);
  assert.equal(kept.images, undefined);
  const path = kept.metadata.framePath as string;
  assert.ok(path.endsWith("/frame.jpg") && existsSync(path));
  assert.match(kept.content, /saved at .*frame\.jpg/);
  rmSync(path.replace(/\/frame\.jpg$/, ""), { recursive: true });
});
