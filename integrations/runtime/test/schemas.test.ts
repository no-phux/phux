import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

import {
  parseAgentEmitResult,
  parseAgentStateList,
  parseAgentSessionOpenResult,
  parseInsertPaneResult,
  parseLaunchResult,
  parseMovePaneResult,
  parseRenderedFrame,
  parseScreenState,
  parseSessionList,
  parseSwapPaneResult,
  parseWatchEvent,
  SchemaValidationError,
} from "../src/schemas.js";

const agentPane = {
  terminal: "@8", session: "work", window: "window-0",
  agent: { id: "pi", label: "Pi", kind: "pi" },
  state: "working", confidence: 1, attention: "normal",
  title: null, cwd: null, sources: [], explanation: "fixture",
};

function inventory(row: unknown) {
  return parseAgentStateList({ schema_version: 1, agents: [row] }).agents[0];
}

test("inventory accepts every canonical Rust producer kind and rejects unknown vocabulary", () => {
  const model = readFileSync(new URL("../../../../crates/phux/src/commands/agent/model.rs", import.meta.url), "utf8");
  const enumBody = /enum AgentKind \{([\s\S]*?)\n\}/.exec(model)?.[1];
  assert.ok(enumBody, "producer enum must remain discoverable");
  const kinds = [...enumBody.matchAll(/^    ([A-Z][A-Za-z]+),$/gm)]
    .map((match) => match[1]!.replace(/([a-z])([A-Z])/g, "$1_$2").toLowerCase());
  assert.deepEqual(kinds, ["codex", "claude", "open_code", "pi", "omp", "plugin", "declared", "unknown"]);
  const parsed = parseAgentStateList({ schema_version: 1, agents: kinds.map((kind) => ({
    ...agentPane, agent: { ...agentPane.agent, kind },
  })) });
  assert.deepEqual(parsed.agents.map((pane) => pane.agent.kind), kinds);
  for (const kind of ["opencode", "future-kind", "", null, 3]) {
    assert.throws(() => inventory({ ...agentPane, agent: { ...agentPane.agent, kind } }), SchemaValidationError);
  }
});

test("inventory retains nullable additive AgentSession identity and rejects malformed drill-in", () => {
  const session = { resource: "@9", provider: "pi", native_id: "pi-session" };
  assert.equal(inventory(agentPane)?.agent_session, undefined);
  assert.equal(inventory({ ...agentPane, agent_session: null })?.agent_session, null);
  assert.deepEqual(inventory({ ...agentPane, agent_session: session })?.agent_session, session);
  assert.equal(inventory({ ...agentPane, agent_session: { ...session, native_id: null } })?.agent_session?.native_id, null);
  for (const invalid of [[], "@9", { ...session, resource: "all" }, { ...session, provider: 3 }, { ...session, native_id: 3 }]) {
    assert.throws(() => inventory({ ...agentPane, agent_session: invalid }), SchemaValidationError);
  }
});

test("session-list parser accepts v2 terminal inventory and normalizes v1", () => {
  assert.deepEqual(parseSessionList({ schema_version: 1, sessions: [] }), {
    schema_version: 1,
    sessions: [],
    terminals: [],
  });
  assert.deepEqual(parseSessionList({
    schema_version: 2,
    sessions: [{ name: "work", windows: 1, attached: false }],
    terminals: ["@3", "devbox/@7"],
  }), {
    schema_version: 2,
    sessions: [{ name: "work", windows: 1, attached: false }],
    terminals: ["@3", "devbox/@7"],
  });
  assert.throws(
    () => parseSessionList({ schema_version: 2, sessions: [] }),
    SchemaValidationError,
  );
});

test("screen parser validates dimensions and normalizes additive fields", () => {
  const screen = parseScreenState({
    schema_version: 1,
    pane: 2,
    cols: 80,
    rows: 1,
    cursor: { x: 0, y: 0, visible: true },
    lines: ["$"],
  });
  assert.deepEqual(screen.scrollback, []);
  assert.throws(() => parseScreenState({ ...screen, rows: 0 }), SchemaValidationError);
});

test("unwrapped snapshots retain physical geometry and upstream truncation evidence", () => {
  const screen = parseScreenState({
    schema_version: 3, pane: 7, cols: 4, rows: 2,
    cursor: { x: 0, y: 1, visible: true },
    lines: ["abcdefgh"], scrollback: [],
    truncated: true, truncated_reason: "row_window",
  });
  assert.deepEqual(screen.lines, ["abcdefgh"]);
  assert.equal(screen.rows, 2);
  assert.equal(screen.truncated, true);
  assert.equal(screen.truncated_reason, "row_window");
});

test("agent session open and emit parsers accept the documented documents", () => {
  assert.deepEqual(parseAgentSessionOpenResult({
    schema_version: 1, resource: "@9", parent: "@3", provider: "pi", native_id: "s-1",
  }), {
    schema_version: 1, resource: "@9", parent: "@3", provider: "pi", native_id: "s-1",
  });
  assert.equal(parseAgentSessionOpenResult({
    schema_version: 1, resource: "@9", parent: "@3", provider: "pi", native_id: null,
  }).native_id, null);
  assert.equal(parseAgentEmitResult({
    schema_version: 1, resource: "@9", seq: 2, ts_ms: 10, type: "ask",
  }).type, "ask");
  assert.throws(() => parseAgentEmitResult({
    schema_version: 1, resource: "@9", seq: 1, ts_ms: 0, type: "not-a-type",
  }), SchemaValidationError);
});

test("new machine parsers reject incompatible versions and malformed event payloads", () => {
  assert.throws(() => parseLaunchResult({
    schema_version: 2, terminal_id: 1, integration: "codex", plugin: "agents", argv: ["codex"],
  }), SchemaValidationError);
  assert.throws(() => parseWatchEvent({ event: "asked", terminal: "@1", id: "q" }), SchemaValidationError);
  assert.throws(() => parseRenderedFrame({
    schema_version: 1, cols: 2, rows: 1, cursor: null, cells: [],
  }), /exactly 2 entries/);
});

test("spatial parsers require canonical operations, fields, directions, and ratios", () => {
  assert.equal(parseInsertPaneResult({
    schema_version: 1, operation: "insert-pane", session_id: 1,
    target_terminal_id: 3, new_terminal_id: 4, direction: "vertical", ratio: 0.4,
  }).new_terminal_id, 4);
  assert.equal(parseMovePaneResult({
    schema_version: 1, operation: "move-pane", session_id: 1,
    source_terminal_id: 4, target_terminal_id: 3, direction: "horizontal", ratio: 0.6,
  }).source_terminal_id, 4);
  assert.equal(parseSwapPaneResult({
    schema_version: 1, operation: "swap-pane", session_id: 1,
    first_terminal_id: 3, second_terminal_id: 4,
  }).second_terminal_id, 4);
  assert.throws(() => parseInsertPaneResult({
    schema_version: 1, operation: "move-pane", session_id: 1,
    target_terminal_id: 3, new_terminal_id: 4, direction: "vertical", ratio: 0.4,
  }), SchemaValidationError);
  assert.throws(() => parseMovePaneResult({
    schema_version: 1, operation: "move-pane", session_id: 1,
    source_terminal_id: 4, target_terminal_id: 3, direction: "diagonal", ratio: 1,
  }), SchemaValidationError);
});
