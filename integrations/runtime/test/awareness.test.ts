import assert from "node:assert/strict";
import test from "node:test";

import type { AgentPane } from "../src/schemas.js";
import {
  PhuxContextAwareness,
  contextAwarenessEnabled,
  normalizeTerminalIdentity,
} from "../src/awareness.js";

const pane: AgentPane = {
  terminal: "@3",
  session: "work",
  window: "window-0",
  agent: { id: "codex", label: "Codex", kind: "codex" },
  state: "working",
  confidence: 0.9,
  attention: "normal",
  title: "screen title is deliberately omitted",
  cwd: "/repo",
  sources: [{ kind: "screen", signal: "secret evidence", confidence: 1, observed: "terminal contents" }],
  explanation: "screen contents are deliberately omitted",
};

function body(text: string): Record<string, unknown> {
  const line = text.split("\n").find((candidate) => candidate.startsWith("{"));
  assert.notEqual(line, undefined);
  return JSON.parse(line as string) as Record<string, unknown>;
}

test("emits one cache-friendly checkpoint, suppresses unchanged state, then emits a delta", async () => {
  let panes: readonly AgentPane[] = [pane];
  const requests: Array<{ timeoutMs?: number; signal?: AbortSignal }> = [];
  const awareness = new PhuxContextAwareness({
    agentList: async (options = {}) => {
      requests.push(options);
      return { schema_version: 1, agents: panes };
    },
  }, { timeoutMs: 321 });
  const signal = new AbortController().signal;

  const first = await awareness.next("pi:one", { self: "65", selected: "@3" }, signal);
  assert.equal(first?.kind, "checkpoint");
  assert.equal(first?.seq, 1);
  const firstBody = body(first?.text ?? "");
  assert.equal(firstBody.self, "@65");
  assert.equal(firstBody.selected, "@3");
  assert.doesNotMatch(first?.text ?? "", /screen title|secret evidence|terminal contents/);
  assert.match(first?.text ?? "", /untrusted observational metadata/);

  assert.equal(await awareness.next("pi:one", { self: "65", selected: "@3" }, signal), null);

  panes = [{ ...pane, state: "idle", attention: "low" }];
  const changed = await awareness.next("pi:one", { self: "65", selected: "@3" }, signal);
  assert.equal(changed?.kind, "delta");
  assert.equal(changed?.seq, 2);
  const changedBody = body(changed?.text ?? "");
  assert.equal(changedBody.base_seq, 1);
  assert.deepEqual(changedBody.removed, undefined);
  assert.equal((changedBody.upsert as Array<{ state: string }>)[0]?.state, "idle");
  assert.equal(requests.every((request) => request.timeoutMs === 321), true);
  assert.equal(requests.every((request) => request.signal === signal), true);
});

test("reports removals, forces post-compaction checkpoints, and isolates streams", async () => {
  let panes: readonly AgentPane[] = [pane];
  const awareness = new PhuxContextAwareness({
    agentList: async () => ({ schema_version: 1, agents: panes }),
  });

  await awareness.next("one");
  const other = await awareness.next("two");
  assert.equal(other?.seq, 1, "each host session owns an independent sequence");
  const compactorOnly = await awareness.checkpoint("two");
  assert.equal(compactorOnly?.seq, 2);
  const afterFailedCompaction = await awareness.next("two");
  assert.equal(afterFailedCompaction?.kind, "checkpoint");
  assert.equal(afterFailedCompaction?.seq, 3, "a compactor-only checkpoint is never the sole durable update");

  panes = [];
  const removed = await awareness.next("one");
  assert.deepEqual(body(removed?.text ?? "").removed, ["@3"]);

  awareness.forceCheckpoint("one");
  const forced = await awareness.next("one");
  assert.equal(forced?.kind, "checkpoint");
  assert.equal(forced?.seq, 3);

  awareness.delete("one");
  const reset = await awareness.next("one");
  assert.equal(reset?.seq, 1);
  assert.equal(reset?.kind, "checkpoint");
});

test("unavailability is bounded, edge-filtered, and recovers with a checkpoint", async () => {
  let available = false;
  const awareness = new PhuxContextAwareness({
    agentList: async () => {
      if (!available) throw new Error(`offline\n${"x".repeat(1_000)}`);
      return { schema_version: 1, agents: [pane] };
    },
  });

  const unavailable = await awareness.next("one");
  assert.equal(body(unavailable?.text ?? "").availability, "unavailable");
  assert.ok(Buffer.byteLength(unavailable?.text ?? "") < 1_024);
  assert.equal(await awareness.next("one"), null);

  available = true;
  const recovered = await awareness.next("one");
  assert.equal(recovered?.kind, "checkpoint");
  assert.equal(body(recovered?.text ?? "").availability, "available");
});

test("caps fleet size and total checkpoint bytes deterministically", async () => {
  const panes = Array.from({ length: 100 }, (_, index): AgentPane => ({
    ...pane,
    terminal: `@${String(index + 1)}`,
    session: `session-${String(index)}-${"s".repeat(100)}`,
    cwd: `/repo/${"x".repeat(500)}`,
  }));
  const awareness = new PhuxContextAwareness({
    agentList: async () => ({ schema_version: 1, agents: panes }),
  }, { maxBytes: 2_048, maxPanes: 10 });

  const emission = await awareness.next("bounded");
  const parsed = body(emission?.text ?? "");
  assert.ok(Buffer.byteLength(emission?.text ?? "") <= 2_048);
  assert.ok((parsed.panes as unknown[]).length <= 10);
  assert.ok((parsed.omitted as number) > 0);
});

test("parses the opt-out environment and canonicalizes inherited terminal ids", () => {
  assert.equal(contextAwarenessEnabled(undefined), true);
  assert.equal(contextAwarenessEnabled("off"), false);
  assert.equal(contextAwarenessEnabled("YES"), true);
  assert.throws(() => contextAwarenessEnabled("sometimes"), /PHUX_CONTEXT_AWARENESS/);
  assert.equal(normalizeTerminalIdentity("65"), "@65");
  assert.equal(normalizeTerminalIdentity("host/@3"), "host/@3");
  assert.equal(normalizeTerminalIdentity(undefined), null);
});
