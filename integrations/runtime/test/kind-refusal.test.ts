import assert from "node:assert/strict";
import test from "node:test";

import { PhuxCli } from "../src/adapter.js";
import { PhuxError } from "../src/errors.js";
import type { ProcessResult, RunRequest } from "../src/runner.js";
import { createPhuxTools } from "../src/tools.js";

// What the CLI prints for each kind (crates/phux/tests/automation_e2e/kind_dispatch_e2e.rs), captured live.
const CONTEXT = { sessionID: "test", messageID: "message", id: "call", agent: "test" };
const SCREEN = { schema_version: 3, pane: 1, cols: 80, rows: 1, cursor: null, lines: ["ready"], scrollback: [] };
const TREE = { schema_version: 1, seq: 1, document: { title: "hello" } };
const TEXT_DOC = { schema_version: 1, seq: 1, bytes: 14, content_type: "text/markdown", format: null, width: null, height: null, text: "# Plan\n## Done" };

function ok(value: unknown, exitCode = 0, stderr = ""): ProcessResult {
  return { termination: "completed", exitCode, stdout: value === undefined ? "" : JSON.stringify(value), stderr };
}

function failed(code: string, message: string, exitCode: number): ProcessResult {
  return ok(undefined, exitCode, JSON.stringify({ schema_version: 1, error: { code, message }, remedy: "", exit_code: exitCode }));
}

/** `phux send-keys` and `phux paste` run without `--json`, so their refusal is diagnostic text. */
function human(message: string, exitCode: number): ProcessResult {
  return { termination: "completed", exitCode, stdout: "", stderr: `phux: error: ${message}\n  hint: try something else\n` };
}

function harness(respond: (request: RunRequest) => ProcessResult) {
  const requests: RunRequest[] = [];
  const cli = new PhuxCli({ runner: async (request) => { requests.push(request); return respond(request); } });
  const tools = createPhuxTools({ cli, getSelectedTarget: () => undefined, selectTarget: () => {} });
  return { requests, tools };
}

async function refusal(tool: string, input: Record<string, unknown>, result: ProcessResult): Promise<PhuxError> {
  const { requests, tools } = harness(() => result);
  const error = await tools[tool]!.execute(input, CONTEXT).then(() => undefined, (cause: unknown) => cause);
  assert.ok(error instanceof PhuxError, `${tool} must reject with a PhuxError, got ${String(error)}`);
  assert.equal(error.code, "wrong_resource_kind", error.message);
  assert.equal(requests.length, 1, "the kind comes from the CLI's own answer, not another inventory call");
  return error;
}

test("phux_snapshot refuses each non-Terminal kind, naming the tool that reads it", async () => {
  const cases: [string, ProcessResult, string | undefined, RegExp][] = [
    ["@2", ok(TREE), "accessibility_tree", /@2 is an accessibility tree: use phux_tree_snapshot$/],
    ["@3", ok(TEXT_DOC), "document", /@3 is a document: `phux snapshot @3` from a shell/],
    ["@5", failed("usage_error", "@5 is a pixel surface: its image goes to a file, not the terminal", 2), "pixel_surface", /@5 is a pixel surface: use phux_surface_snapshot$/],
    ["@6", failed("wrong_resource_kind", "`snapshot` does not apply to @6, an agent session", 2), "agent_session", /@6 is an agent session: read an agent session with `phux agent log`/],
    ["@7", failed("wrong_resource_kind", "`snapshot` does not apply to @7, a resource of kind hologram", 2), "hologram", /@7 is a resource of kind hologram: address a terminal/],
  ];
  for (const [target, result, kind, message] of cases) {
    const error = await refusal("phux_snapshot", { target }, result);
    assert.match(error.message, /^phux_snapshot reads a terminal's screen, and /);
    assert.match(error.message, message);
    assert.equal(error.resourceKind, kind, target);
  }
  // A screen option on a document is the CLI's usage error; it still names the reader.
  const tail = await refusal("phux_snapshot", { target: "@2", tail: 5 }, failed("usage_error", "`--tail` applies to a terminal only, and @2 is an accessibility tree", 2));
  assert.match(tail.message, /use phux_tree_snapshot$/);
  assert.equal(tail.exitCode, 2, "the CLI's exit status and error document stay available");
  assert.deepEqual((tail.cliError?.["error"] as { code: string }).code, "usage_error");
});

test("phux_wait refuses each non-Terminal kind, including a document wait the CLI ran", async () => {
  const cases: [string, Record<string, unknown>, ProcessResult, string | undefined, RegExp][] = [
    ["matched tree", { until: "hello" }, ok(TREE), "accessibility_tree", /@2 is an accessibility tree: use phux_tree_wait/],
    ["matched document", { regex: "^## Done$" }, ok(TEXT_DOC), "document", /@2 is a document: use phux_tree_wait/],
    ["timed out", { until: "never" }, failed("timed_out", "wait timed out after 1 document of @2", 124), undefined, /@2 is an accessibility tree or a typed document: use phux_tree_wait/],
    ["closed", { until: "never" }, failed("workload", "@2 closed before a document matched", 1), undefined, /@2 is an accessibility tree or a typed document: use phux_tree_wait/],
    ["idle", {}, failed("usage_error", "@2 is an accessibility tree, which has no idle: a wait on it matches text", 2), "accessibility_tree", /use phux_tree_wait/],
    ["binary", { until: "x" }, failed("usage_error", "@2 is a application/octet-stream document, not text; a wait matches text", 2), "document", /@2 is a document: use phux_tree_wait/],
    ["surface", { until: "x" }, failed("wrong_resource_kind", "`wait` does not apply to @2, a pixel surface", 2), "pixel_surface", /@2 is a pixel surface: use phux_surface_snapshot$/],
    ["agent session", { until: "x" }, failed("wrong_resource_kind", "`wait` does not apply to @2, an agent session", 2), "agent_session", /`phux agent log`/],
  ];
  for (const [name, input, result, kind, message] of cases) {
    const error = await refusal("phux_wait", { target: "@2", timeout_seconds: 1, ...input }, result);
    assert.match(error.message, /^phux_wait waits on a terminal's screen, and /, name);
    assert.match(error.message, message, name);
    assert.equal(error.resourceKind, kind, name);
  }
});

test("phux_send_keys and phux_paste refuse every non-Terminal kind", async () => {
  for (const [label, kind] of [["an accessibility tree", "accessibility_tree"], ["a document", "document"], ["a pixel surface", "pixel_surface"]] as const) {
    const error = await refusal("phux_send_keys", { target: "@2", keys: ["Enter"] }, human(`\`send-keys\` does not apply to @2, ${label}`, 2));
    assert.match(error.message, new RegExp(`^phux_send_keys types into a terminal, and @2 is ${label}: keys reach a terminal only`));
    assert.match(error.message, /`phux agent page type TEXT`/);
    assert.equal(error.resourceKind, kind);
  }
  const session = await refusal("phux_send_keys", { target: "@6", keys: ["Enter"] }, human("`send-keys` does not apply to @6, an agent session", 2));
  assert.match(session.message, /address its parent pane/);
  const paste = await refusal("phux_paste", { target: "@2", text: "hi" }, human("paste refused for '@2': resource is accessibility-tree, operation requires terminal (try `phux ls`)", 1));
  assert.match(paste.message, /^phux_paste types into a terminal, and @2 is an accessibility tree: keys reach a terminal only/);
  assert.equal(paste.resourceKind, "accessibility_tree");
});

test("Terminal reads, waits and keys are unchanged", async () => {
  const { requests, tools } = harness((request) => {
    if (request.args[0] === "wait") return ok(SCREEN, 124, JSON.stringify({ schema_version: 1, error: { code: "timed_out", message: "wait timed out after 7 polls" } }));
    return request.args[0] === "send-keys" ? ok(undefined) : ok(SCREEN);
  });
  assert.match((await tools.phux_snapshot!.execute({ target: "@1" }, CONTEXT)).content, /^snapshot target=@1 80x1\nready$/);
  const waited = await tools.phux_wait!.execute({ target: "@1", until: "never", timeout_seconds: 1 }, CONTEXT);
  assert.equal(waited.metadata.outcome, "timed_out");
  await tools.phux_send_keys!.execute({ target: "@1", keys: ["Enter"] }, CONTEXT);
  assert.deepEqual(requests.map((request) => request.args[0]), ["snapshot", "wait", "send-keys"]);
});

test("a Terminal's own failures are not mistaken for a kind refusal", async () => {
  const cases: [string, Record<string, unknown>, ProcessResult][] = [
    ["phux_snapshot", { target: "@1" }, failed("usage_error", "`--out` writes a document's content to a file, and @1 is a terminal", 2)],
    ["phux_wait", { target: "@1", regex: "(" }, failed("usage_error", "invalid regex: a document is not text", 2)],
    ["phux_wait", { target: "@1", until: "x" }, ok(undefined, 124)],
    ["phux_send_keys", { target: "@1", keys: ["Enter"] }, human("no such target @1", 1)],
  ];
  for (const [tool, input, result] of cases) {
    const { tools } = harness(() => result);
    await assert.rejects(tools[tool]!.execute(input, CONTEXT), (error: unknown) => error instanceof PhuxError && error.code === "command_failed", tool);
  }
  // A screen-shaped answer that fails validation is still an invalid response.
  const { tools } = harness(() => ok({ ...SCREEN, cols: "wide" }));
  await assert.rejects(tools.phux_snapshot!.execute({ target: "@1" }, CONTEXT), (error: unknown) => error instanceof PhuxError && error.code === "invalid_response");
});
