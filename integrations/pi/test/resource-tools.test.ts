import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import type { AgentToolResult, ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { PhuxCli } from "../src/adapter.js";
import {
  PhuxDocumentsParams,
  PhuxListParams,
  PhuxSurfaceSnapshotParams,
  PhuxTreeSnapshotParams,
  PhuxTreeWaitParams,
  registerPhuxDocumentTools,
  registerPhuxListTool,
} from "../src/resource-tools.js";
import type { ProcessResult, RunRequest } from "../src/runner.js";
import { MAX_MODEL_BYTES, type PhuxToolDetails } from "../src/tools.js";

// Saved frames and trees go to a private directory under TMPDIR; isolate it.
process.env.TMPDIR = mkdtempSync(join(tmpdir(), "phux-pi-test-"));

type Execute = (id: string, params: Record<string, unknown>, signal?: AbortSignal) => Promise<AgentToolResult<PhuxToolDetails>>;

const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==", "base64");
const LIST = {
  schema_version: 4,
  sessions: [{ name: "work", windows: 1, attached: false }],
  terminals: ["@1"],
  unreachable: [],
  resources: [
    { id: "@1", kind: "terminal", parent: null, lifecycle: "running", exit: null, key: null },
    { id: "@2", kind: "accessibility_tree", name: "Safari", parent: "@1", lifecycle: "running", exit: null, producer: "connected", key: null },
    { id: "@3", kind: "pixel_surface", name: "Safari", parent: "@1", lifecycle: "running", exit: null, producer: "gone", control: "page", key: null },
    { id: "@4", kind: "hologram", parent: "@1" },
  ],
};

function completed(value: unknown, exitCode = 0, stderr = ""): ProcessResult {
  return { termination: "completed", exitCode, stdout: value === undefined ? "" : JSON.stringify(value), stderr };
}

function fixture(respond: (request: RunRequest) => ProcessResult) {
  const requests: RunRequest[] = [];
  const tools = new Map<string, { parameters: unknown; execute: Execute }>();
  const cli = new PhuxCli({ socket: "/tmp/phux.sock", runner: async (request) => { requests.push(request); return respond(request); } });
  const api = { registerTool: (definition: { name: string; parameters: unknown; execute: Execute }) => tools.set(definition.name, definition) } as unknown as ExtensionAPI;
  registerPhuxListTool(api, cli);
  registerPhuxDocumentTools(api, cli);
  const run = (name: string, params: Record<string, unknown>) => tools.get(name)!.execute(name, params);
  return { requests, tools, run };
}

const text = (result: AgentToolResult<PhuxToolDetails>): string =>
  result.content[0]?.type === "text" ? result.content[0].text : "";

test("resource tool schemas are strict and require exact document targets", () => {
  for (const schema of [PhuxListParams, PhuxDocumentsParams, PhuxTreeSnapshotParams, PhuxTreeWaitParams, PhuxSurfaceSnapshotParams]) {
    assert.equal((schema as { additionalProperties?: boolean }).additionalProperties, false);
  }
  assert.deepEqual((PhuxTreeWaitParams as { required?: string[] }).required, ["target", "until"]);
  assert.equal((PhuxTreeSnapshotParams as unknown as { properties: { target: { pattern: string } } }).properties.target.pattern, "^(?:[^/\\s]+/)?@[0-9]+$");
  assert.deepEqual([...fixture(() => completed({})).tools.keys()], [
    "phux_list", "phux_documents", "phux_tree_snapshot", "phux_tree_wait", "phux_surface_snapshot",
  ]);
});

test("phux_list prints every resource kind beside the sessions", async () => {
  const { run } = fixture(() => completed(LIST));
  const listed = await run("phux_list", {});
  assert.equal(text(listed), [
    "sessions=1 resources=4",
    "work\twindows=1\tattached=false",
    "@1\tkind=terminal\tparent=-\tlifecycle=running",
    "@2\tkind=accessibility_tree\tparent=@1\tname=\"Safari\"\tlifecycle=running\tproducer=connected",
    "@3\tkind=pixel_surface\tparent=@1\tname=\"Safari\"\tlifecycle=running\tproducer=gone\tcontrol=page",
    "@4\tkind=hologram\tparent=@1\topaque",
  ].join("\n"));
  assert.equal(listed.details.summary, "1 session(s), 4 resource(s)");
});

test("phux_list keeps the header, sessions and first resources of a large fleet", async () => {
  const resources = Array.from({ length: 400 }, (_, index) => ({ id: `@${index + 1}`, kind: "terminal", name: `pane ${index + 1}`, parent: null }));
  const sessions = Array.from({ length: 15 }, (_, index) => ({ name: `session-${index}`, windows: 1, attached: false }));
  const { run } = fixture(() => completed({ ...LIST, sessions, resources }));
  const listed = text(await run("phux_list", {}));
  const lines = listed.split("\n");
  assert.ok(Buffer.byteLength(listed) <= MAX_MODEL_BYTES);
  assert.equal(lines[0], "sessions=15 resources=400");
  assert.equal(lines[1], "session-0\twindows=1\tattached=false");
  assert.equal(lines[16], '@1\tkind=terminal\tparent=-\tname="pane 1"\tlifecycle=running');
  assert.match(lines.at(-1)!, /truncated the list to its first/);
});

test("document tools call the CLI verbs and return the shared runtime's results", async () => {
  const { requests, run } = fixture((request) => {
    if (request.args[0] === "ls") return completed(LIST);
    if (request.args[2] === "snapshot" && request.args[1] === "tree") return completed({ schema_version: 1, seq: 2, document: { title: "hello" } });
    if (request.args[2] === "follow") return completed(undefined, 1, JSON.stringify({ error: { code: "workload", message: "no document matched within 7s" } }));
    const out = request.args.find((arg) => arg.startsWith("--out="))!.slice("--out=".length);
    writeFileSync(out, PNG);
    return completed({ schema_version: 1, seq: 1, bytes: PNG.length, format: "png", width: 1, height: 1, content_type: null, out });
  });
  const documents = await run("phux_documents", {});
  assert.match(text(documents), /"id": "@2"[\s\S]*"control": "page"/);
  assert.doesNotMatch(text(documents), /hologram|"id": "@1"/);

  const tree = await run("phux_tree_snapshot", { target: "@2" });
  assert.equal(text(tree), 'tree_snapshot target=@2 seq=2\n{"title":"hello"}');
  assert.equal(tree.details.operation, "tree_snapshot");
  assert.equal(tree.details.target, "@2");

  const waited = await run("phux_tree_wait", { target: "@2", until: "OK", timeout_seconds: 7 });
  await run("phux_tree_wait", { target: "@2", until: "OK" });
  assert.equal(waited.details.summary, "tree_wait target=@2 outcome=timed_out");

  const surface = await run("phux_surface_snapshot", { target: "@3" });
  assert.deepEqual(surface.content[1], { type: "image", data: PNG.toString("base64"), mimeType: "image/png" });
  assert.equal(surface.details.summary, "surface_snapshot target=@3 seq=1 bytes=70 format=png 1x1");

  assert.deepEqual(requests.map((request) => request.args.slice(0, 4)), [
    ["ls", "--json", "--socket", "/tmp/phux.sock"],
    ["agent", "tree", "snapshot", "--json"],
    ["agent", "tree", "follow", "--json"],
    ["agent", "tree", "follow", "--json"],
    ["agent", "surface", "snapshot", "--json"],
  ]);
  assert.deepEqual(requests[3]?.args.slice(5, 7), ["--timeout", "60"], "Pi shares MCP's 60-second default");
  assert.deepEqual(requests[2]?.args.slice(4), ["--until=OK", "--timeout", "7", "--socket", "/tmp/phux.sock", "--", "@2"]);
  await assert.rejects(run("phux_tree_snapshot", { target: "alias:browser" }), /exact @N/);
});
