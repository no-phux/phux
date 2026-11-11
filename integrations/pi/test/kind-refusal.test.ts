import assert from "node:assert/strict";
import test from "node:test";

import type { AgentToolResult, ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { PhuxCli } from "../src/adapter.js";
import { PhuxError } from "../src/errors.js";
import type { ProcessResult, RunRequest } from "../src/runner.js";
import { PhuxTargetStore } from "../src/target-store.js";
import { registerPhuxTools, type PhuxToolDetails } from "../src/tools.js";

// Pi's Terminal tools share the runtime adapter, so a non-Terminal target is
// refused with wrong_resource_kind, from what the CLI printed for it.
type Execute = (id: string, params: Record<string, unknown>, signal?: AbortSignal) => Promise<AgentToolResult<PhuxToolDetails>>;

const TREE = { schema_version: 1, seq: 1, document: { title: "hello" } };
const TEXT_DOC = { schema_version: 1, seq: 1, bytes: 4, content_type: "text/plain", format: null, width: null, height: null, text: "note" };

function ok(value: unknown, exitCode = 0): ProcessResult {
  return { termination: "completed", exitCode, stdout: value === undefined ? "" : JSON.stringify(value), stderr: "" };
}

function failed(code: string, message: string, exitCode: number): ProcessResult {
  return { termination: "completed", exitCode, stdout: "", stderr: JSON.stringify({ schema_version: 1, error: { code, message }, remedy: "", exit_code: exitCode }) };
}

function human(message: string, exitCode: number): ProcessResult {
  return { termination: "completed", exitCode, stdout: "", stderr: `phux: error: ${message}\n` };
}

async function refused(tool: string, params: Record<string, unknown>, result: ProcessResult): Promise<PhuxError> {
  const requests: RunRequest[] = [];
  const cli = new PhuxCli({ socket: "/tmp/phux.sock", runner: async (request) => { requests.push(request); return result; } });
  const tools = new Map<string, Execute>();
  const api = { registerTool: (definition: { name: string; execute: Execute }) => tools.set(definition.name, definition.execute) } as unknown as ExtensionAPI;
  registerPhuxTools(api, cli, new PhuxTargetStore({ appendEntry: () => {} }, cli));
  const error = await tools.get(tool)!(tool, params).then(() => undefined, (cause: unknown) => cause);
  assert.ok(error instanceof PhuxError, `${tool}: ${String(error)}`);
  assert.equal(error.code, "wrong_resource_kind", error.message);
  assert.equal(requests.length, 1);
  return error;
}

test("phux_snapshot refuses a tree, a document and a surface, naming the reader", async () => {
  assert.match((await refused("phux_snapshot", { target: "@2" }, ok(TREE))).message, /@2 is an accessibility tree: use phux_tree_snapshot$/);
  assert.match((await refused("phux_snapshot", { target: "@3" }, ok(TEXT_DOC))).message, /@3 is a document: `phux snapshot @3` from a shell/);
  const surface = await refused("phux_snapshot", { target: "@5" }, failed("usage_error", "@5 is a pixel surface: its image goes to a file, not the terminal", 2));
  assert.match(surface.message, /use phux_surface_snapshot$/);
  assert.equal(surface.resourceKind, "pixel_surface");
});

test("phux_wait refuses a document wait and a surface, naming phux_tree_wait", async () => {
  assert.match((await refused("phux_wait", { target: "@2", until: "hello" }, ok(TREE))).message, /^phux_wait waits on a terminal's screen, and @2 is an accessibility tree: use phux_tree_wait/);
  assert.match((await refused("phux_wait", { target: "@3", until: "x" }, failed("timed_out", "wait timed out after 2 documents of @3", 124))).message, /@3 is an accessibility tree or a typed document: use phux_tree_wait/);
  assert.match((await refused("phux_wait", { target: "@5", until: "x" }, failed("wrong_resource_kind", "`wait` does not apply to @5, a pixel surface", 2))).message, /use phux_surface_snapshot$/);
});

test("phux_send_keys and phux_paste refuse a document's input", async () => {
  for (const label of ["an accessibility tree", "a document", "a pixel surface"]) {
    const error = await refused("phux_send_keys", { target: "@2", keys: ["Enter"] }, human(`\`send-keys\` does not apply to @2, ${label}`, 2));
    assert.match(error.message, /keys reach a terminal only; a document takes typed intents through its producer/);
  }
  const paste = await refused("phux_paste", { target: "@3", text: "hi" }, human("paste refused for '@3': resource is document, operation requires terminal (try `phux ls`)", 1));
  assert.equal(paste.resourceKind, "document");
});
