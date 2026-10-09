import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

import type { AgentEmitOptions, AgentSessionOpenOptions, ExecutionOptions } from "../src/adapter.js";
import { PhuxError } from "../src/errors.js";
import {
  PhuxLifecycle,
  registerPhuxLifecycle,
  PhuxLifecycleShutdownError,
  type LifecycleCommandOptions,
  type LifecycleTimers,
  type PhuxLifecycleAdapter,
} from "../src/lifecycle.js";
import type {
  AgentEmitResult,
  AgentEventType,
  AgentPane,
  AgentRecord,
  AgentSessionCloseResult,
  AgentSessionOpenResult,
  AgentSessionIdentity,
  AgentStateList,
} from "../src/schemas.js";
import { PhuxTargetStore, type PhuxTargetSelection } from "../src/target-store.js";

const target: PhuxTargetSelection = {
  version: 1,
  selector: "@3",
  session: "work",
  window: "window-0",
  display: "work:window-0 @3",
};

const targetB: PhuxTargetSelection = {
  version: 1,
  selector: "@4",
  session: "other",
  window: "window-1",
  display: "other:window-1 @4",
};

class FakeTimers implements LifecycleTimers {
  private next = 0;
  private readonly callbacks = new Map<number, () => void>();

  setTimeout(callback: () => void): number {
    const id = ++this.next;
    this.callbacks.set(id, callback);
    return id;
  }

  clearTimeout(handle: unknown): void {
    this.callbacks.delete(handle as number);
  }

  runAll(): void {
    const pending = [...this.callbacks.values()];
    this.callbacks.clear();
    for (const callback of pending) callback();
  }
}

class FakeAdapter implements PhuxLifecycleAdapter {
  readonly sets: Array<{ target: string; record: AgentRecord }> = [];
  readonly shows: string[] = [];
  readonly clears: string[] = [];
  readonly opens: Array<{ target: string; provider: string; nativeId?: string }> = [];
  readonly emits: Array<{ target: string; type: AgentEventType; data?: Readonly<Record<string, unknown>> }> = [];
  readonly sessionCloses: string[] = [];
  readonly commandOptions: Array<{ timeoutMs?: number; signal?: AbortSignal }> = [];
  record: AgentRecord | null = null;
  sessionIdentity: AgentSessionIdentity | null = null;
  failSet = false;
  failOpen: Error | null = null;
  seq = 0;

  async agentSet(
    selector: string,
    record: AgentRecord,
    options: LifecycleCommandOptions,
  ): Promise<AgentRecord> {
    this.commandOptions.push(options);
    this.sets.push({ target: selector, record });
    if (this.failSet) throw new Error("phux absent");
    this.record = record;
    return record;
  }

  async agentShow(
    options: LifecycleCommandOptions & { readonly target: string },
  ): Promise<AgentStateList> {
    this.commandOptions.push(options);
    this.shows.push(options.target);
    const state = projection(this.record);
    return { ...state, agents: state.agents.map((pane) => ({ ...pane, agent_session: this.sessionIdentity })) };
  }

  async agentClear(selector: string, options: LifecycleCommandOptions): Promise<void> {
    this.commandOptions.push(options);
    this.clears.push(selector);
    this.record = null;
  }

  async agentSessionOpen(
    selector: string,
    options: AgentSessionOpenOptions,
  ): Promise<AgentSessionOpenResult> {
    this.commandOptions.push(options);
    this.opens.push({
      target: selector,
      provider: options.provider,
      ...(options.nativeId === undefined ? {} : { nativeId: options.nativeId }),
    });
    if (this.failOpen !== null) throw this.failOpen;
    this.sessionIdentity = { resource: "@99", provider: options.provider, native_id: options.nativeId ?? null };
    return { schema_version: 1, parent: selector, ...this.sessionIdentity };
  }

  async agentEmit(
    selector: string,
    type: AgentEventType,
    options: AgentEmitOptions = {},
  ): Promise<AgentEmitResult> {
    this.commandOptions.push(options);
    this.emits.push({
      target: selector,
      type,
      ...(options.data === undefined ? {} : { data: options.data }),
    });
    this.seq += 1;
    return { schema_version: 1, resource: "@99", seq: this.seq, ts_ms: this.seq, type };
  }

  async agentSessionClose(
    selector: string,
    options: ExecutionOptions = {},
  ): Promise<AgentSessionCloseResult> {
    this.commandOptions.push(options);
    this.sessionCloses.push(selector);
    return { resource: selector, closed: true };
  }
}

function projection(record: AgentRecord | null): AgentStateList {
  const source = record === null ? [] : [{
    kind: "agent_record",
    signal: "phux.agent/v1 metadata record",
    confidence: 0.98,
    observed: JSON.stringify(record),
  }];
  return {
    schema_version: 1,
    agents: [{
      terminal: target.selector,
      session: target.session,
      window: target.window,
      agent: { id: "pi", label: "pi", kind: "declared" },
      state: record?.state ?? "unknown",
      confidence: 0.98,
      attention: record?.attention ?? "normal",
      title: null,
      cwd: null,
      sources: source,
      explanation: "test",
    }],
  };
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

test("overlapping generations are serialized and end at the newest state", async () => {
  const timers = new FakeTimers();
  const calls: AgentRecord[] = [];
  let releaseFirst: (() => void) | undefined;
  const first = new Promise<void>((resolve) => { releaseFirst = resolve; });
  const adapter: PhuxLifecycleAdapter = {
    agentShow: async () => projection(null),
    agentClear: async () => {},
    agentSet: async (_selector, record) => {
      calls.push(record);
      if (calls.length === 1) await first;
      return record;
    },
  };
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers, debounceMs: 5 });

  lifecycle.start("session-1", target);
  timers.runAll();
  await flush();
  assert.equal(calls.length, 1);

  lifecycle.setTarget(targetB);
  timers.runAll();
  await flush();
  assert.equal(calls.length, 1, "the second write waits for the first");
  releaseFirst?.();
  await lifecycle.settled();

  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1], {
    name: "pi",
    kind: "pi",
    session: "pi:session-1",
  });
});

/**
 * phux-w7z2.38. A declared `state` outranks the server's derivation for the
 * record's whole lifetime (docs/spec/L3.md 3.7, ADR-0046 point 8), so reporting
 * one here stood `rules/pi.toml` down on every pane running this extension.
 *
 * The write count is half the assertion. Writing identity on every turn would
 * be equally wrong: SET_METADATA replaces the record wholesale, so each write
 * carries `state: "unknown"` and clobbers the derived state, publishing a
 * `working -> unknown` edge that `phux agent wait` reads as the agent departing
 * (phux-w7z2.37).
 */
test("the record is identity only, and only owner or target changes write it", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers });

  lifecycle.start("session-1", target);
  timers.runAll();
  await lifecycle.settled();
  assert.equal(adapter.sets.length, 1, "one write for the session");
  assert.equal(adapter.opens.length, 1, "one AgentSession open for the pane");

  for (const record of adapter.sets.map((entry) => entry.record)) {
    assert.equal(record.state, undefined, "a declared state stands the detector down");
    assert.equal(record.attention, undefined, "attention derives from state");
  }

  // Re-declaring the same identity must not produce a second write: the
  // reconciler compares bindings, and identity is the whole binding now.
  lifecycle.setTarget(target);
  timers.runAll();
  await lifecycle.settled();
  assert.equal(adapter.sets.length, 1, "an unchanged target must not rewrite");

  // A real target change is the one thing that does write again.
  lifecycle.setTarget(targetB);
  timers.runAll();
  await lifecycle.settled();
  assert.equal(adapter.sets.length, 2, "a moved pane needs the record on the new one");
  assert.equal(adapter.sets[1]?.record.state, undefined);
});

test("phux failures stay best-effort and a later transition retries", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  adapter.failSet = true;
  const errors: unknown[] = [];
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers, onError: (error) => errors.push(error) });

  lifecycle.start("session-1", target);
  timers.runAll();
  await lifecycle.settled();
  assert.equal(errors.length, 1);

  adapter.failSet = false;
  lifecycle.setTarget(targetB);
  timers.runAll();
  await lifecycle.settled();
  assert.equal(adapter.sets.length, 2);
  assert.equal(adapter.record?.session, "pi:session-1");
});

test("every lifecycle CLI command receives the configured local timeout and signal", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers, timeoutMs: 321 });
  lifecycle.start("session-1", target);
  timers.runAll();
  await lifecycle.settled();
  await lifecycle.shutdown();

  assert.ok(adapter.commandOptions.length >= 3);
  assert.ok(adapter.commandOptions.every((options) => options.timeoutMs === 321));
  assert.ok(adapter.commandOptions.every((options) => options.signal instanceof AbortSignal));
});

test("shutdown aborts a hanging command and returns at its bounded deadline", async () => {
  const timers = new FakeTimers();
  const errors: unknown[] = [];
  let commandSignal: AbortSignal | undefined;
  const never = new Promise<AgentRecord>(() => {});
  const adapter: PhuxLifecycleAdapter = {
    agentShow: async () => projection(null),
    agentClear: async () => {},
    agentSet: async (_selector, _record, options) => {
      commandSignal = options.signal;
      return never;
    },
  };
  const lifecycle = new PhuxLifecycle({
    cli: adapter,
    timers,
    timeoutMs: 50,
    onError: (error) => errors.push(error),
  });
  lifecycle.start("session-1", target);
  timers.runAll();
  await flush();

  const shutdown = lifecycle.shutdown();
  assert.equal(commandSignal?.aborted, true);
  timers.runAll();
  await shutdown;

  assert.ok(errors.some((error) => error instanceof PhuxLifecycleShutdownError));
});

test("partial foreign provenance is released so a target switch can continue", async () => {
  const timers = new FakeTimers();
  const order: string[] = [];
  const adapter: PhuxLifecycleAdapter = {
    agentSet: async (selector, record) => {
      order.push(`set:${selector}`);
      return record;
    },
    agentShow: async () => {
      order.push("show:@3");
      const base = projection(null);
      const pane = base.agents[0];
      assert.ok(pane !== undefined);
      return {
        schema_version: 1,
        agents: [{
          ...pane,
          sources: [{
            kind: "agent_record",
            signal: "phux.agent/v1 metadata record",
            confidence: 0.98,
            observed: JSON.stringify({ name: "pi", kind: "pi" }),
          }],
        }],
      };
    },
    agentClear: async (selector) => {
      order.push(`clear:${selector}`);
    },
  };
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers });
  lifecycle.start("session-1", target);
  timers.runAll();
  await lifecycle.settled();

  lifecycle.setTarget(targetB);
  timers.runAll();
  await lifecycle.settled();

  assert.deepEqual(order, ["set:@3", "show:@3", "set:@4"]);
});

test("shutdown reads provenance and does not clear another owner's record", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers });
  lifecycle.start("ours", target);
  timers.runAll();
  await lifecycle.settled();
  adapter.record = {
    name: "pi",
    kind: "pi",
    state: "idle",
    attention: "low",
    session: "pi:someone-else",
  };

  await lifecycle.shutdown();

  assert.deepEqual(adapter.shows, ["@3"]);
  assert.deepEqual(adapter.clears, []);
});

test("reload verifies and preserves its own declaration and exact AgentSession", async () => {
  const adapter = new FakeAdapter();
  adapter.record = { name: "pi", kind: "pi", session: "pi:session-1" };
  adapter.sessionIdentity = { resource: "@71", provider: "pi", native_id: "session-1" };
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers: new FakeTimers() });
  lifecycle.start("session-1", target, true);
  lifecycle.emit("prompt");
  await lifecycle.settled();
  assert.deepEqual(adapter.sets, []);
  assert.deepEqual(adapter.opens, []);
  assert.deepEqual(adapter.emits, [{ target: "@71", type: "prompt" }]);
  // A new session has replaced the adopted child. Neither subsequent events
  // nor cleanup may resolve the pane's new child by accident.
  adapter.sessionIdentity = { resource: "@72", provider: "claude", native_id: "foreign" };
  lifecycle.emit("stop");
  await lifecycle.settled();
  await lifecycle.shutdown();
  assert.equal(adapter.emits.every((event) => event.target === "@71"), true);
  assert.deepEqual(adapter.sessionCloses, ["@71"]);
  assert.equal(adapter.sessionIdentity.resource, "@72");
});

test("reload establishes missing host identity without touching the old selected sibling", async () => {
  for (const hasOwnRecord of [false, true]) {
    const adapter = new FakeAdapter();
    if (hasOwnRecord) adapter.record = { name: "pi", kind: "pi", session: "pi:session-1" };
    const lifecycle = new PhuxLifecycle({ cli: adapter, timers: new FakeTimers() });
    lifecycle.start("session-1", target, true);
    await lifecycle.settled();
    assert.equal(adapter.sets.length, hasOwnRecord ? 0 : 1);
    assert.deepEqual(adapter.opens.map((open) => open.target), ["@3"]);
    lifecycle.emit("prompt");
    await lifecycle.settled();
    await lifecycle.shutdown();
    assert.equal(adapter.emits.every((event) => event.target === "@99"), true);
    assert.deepEqual(adapter.clears, ["@3"]);
    assert.deepEqual(adapter.sessionCloses, ["@99"]);
  }
});

test("reload refuses absent, foreign or mismatched ownership without any session writes", async () => {
  const own = { name: "pi", kind: "pi", session: "pi:session-1" };
  const ownSession = { resource: "@71", provider: "pi", native_id: "session-1" };
  const cases = [
    { record: null, session: ownSession },
    { record: own, session: { ...ownSession, provider: "claude" } },
    { record: own, session: { ...ownSession, native_id: "other-session" } },
    { record: { ...own, session: "pi:other-session" }, session: ownSession },
    { record: { ...own, name: "claude" }, session: null },
  ];
  for (const fixture of cases) {
    const adapter = new FakeAdapter();
    adapter.record = fixture.record;
    adapter.sessionIdentity = fixture.session;
    const errors: unknown[] = [];
    const lifecycle = new PhuxLifecycle({ cli: adapter, timers: new FakeTimers(), onError: (error) => errors.push(error) });
    lifecycle.start("session-1", target, true);
    lifecycle.emit("prompt");
    await lifecycle.settled();
    await lifecycle.shutdown();
    assert.equal(errors.length, 1);
    assert.deepEqual(adapter.sets, []);
    assert.deepEqual(adapter.opens, []);
    assert.deepEqual(adapter.emits, []);
    assert.deepEqual(adapter.clears, []);
    assert.deepEqual(adapter.sessionCloses, []);
  }
});

test("reload without verifiable inventory does not invent adoption", async () => {
  for (const agents of [[], projection(null).agents]) {
    const adapter = new FakeAdapter();
    adapter.agentShow = async () => ({ schema_version: 1, agents });
    const errors: unknown[] = [];
    const lifecycle = new PhuxLifecycle({ cli: adapter, timers: new FakeTimers(), onError: (error) => errors.push(error) });
    lifecycle.start("session-1", target, true);
    lifecycle.emit("prompt");
    await lifecycle.settled();
    await lifecycle.shutdown();
    assert.equal(errors.length, 1);
    assert.deepEqual(adapter.sets, []);
    assert.deepEqual(adapter.emits, []);
    assert.deepEqual(adapter.sessionCloses, []);
  }
});

test("reload cancels resources without clear or re-set flicker", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers });

  lifecycle.start("session-1", target, true);
  timers.runAll();
  await lifecycle.shutdown(true);

  assert.equal(adapter.sets.length, 0);
  assert.equal(adapter.shows.length, 0);
  assert.equal(adapter.clears.length, 0);
});

test("true target departure and quit clear only the owned declaration", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers });
  lifecycle.start("session-1", target);
  timers.runAll();
  await lifecycle.settled();

  lifecycle.setTarget(null);
  timers.runAll();
  await lifecycle.settled();

  assert.deepEqual(adapter.clears, ["@3"]);
  assert.equal(adapter.record, null);
  await lifecycle.shutdown();
  assert.deepEqual(adapter.clears, ["@3"]);
});

test("per-turn events emit on the AgentSession stream and never rewrite identity", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const handlers = new Map<string, (event: unknown, ctx: unknown) => unknown>();
  const pi = {
    on: (name: string, handler: (event: unknown, ctx: unknown) => unknown) => handlers.set(name, handler),
  } as unknown as ExtensionAPI;
  const pane = projection(null).agents[0] as AgentPane;
  const store = new PhuxTargetStore({ appendEntry: () => {} }, { agentList: async () => ({ agents: [pane] }) });
  await store.refresh();
  store.select({ ...pane, terminal: "@4", session: "other", window: "window-1" });
  const registered = registerPhuxLifecycle(pi, store, { cli: adapter, timers, hostTerminal: "3" });
  const ctx = {
    sessionManager: { getSessionId: () => "session-1" },
  } as unknown as ExtensionContext;

  handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, ctx);
  timers.runAll();
  await registered.lifecycle.settled();
  const afterStart = adapter.sets.length;
  assert.equal(afterStart, 1, "session start declares identity once");
  assert.equal(adapter.sets.at(-1)?.record.state, undefined);
  assert.equal(adapter.opens.length, 1);
  assert.equal(adapter.sets[0]?.target, "@3", "identity belongs to host, not selected @4");
  assert.equal(adapter.opens[0]?.target, "@3");
  store.select({ ...pane, terminal: "@5" });
  timers.runAll();
  await registered.lifecycle.settled();
  assert.equal(adapter.opens.length, 1, "control selection must not rebind AgentSession");

  handlers.get("agent_start")?.({ type: "agent_start" }, ctx);
  handlers.get("project_trust")?.({ type: "project_trust", cwd: "/repo" }, ctx);
  handlers.get("agent_settled")?.({ type: "agent_settled" }, ctx);
  await registered.lifecycle.settled();

  assert.equal(adapter.sets.length, afterStart, "a turn must not rewrite the identity record");
  assert.deepEqual(adapter.emits.map((entry) => entry.type), [
    "session_start",
    "prompt",
    "ask",
    "stop",
  ]);
  assert.equal(adapter.emits.every((entry) => entry.target === "@99"), true);
  assert.equal(adapter.emits.find((entry) => entry.type === "ask")?.data?.kind, "trust");
  assert.equal(adapter.sets.at(-1)?.record.state, undefined);

  const shutdown = handlers.get("session_shutdown")?.(
    { type: "session_shutdown", reason: "reload" },
    ctx,
  );
  await Promise.resolve(shutdown);
  const writesAtShutdown = adapter.sets.length;
  store.select({ ...pane, terminal: "@4", session: "other", window: "window-1" });
  timers.runAll();
  await registered.lifecycle.settled();
  assert.equal(adapter.sets.length, writesAtShutdown, "control changes never write identity");
  assert.equal(adapter.clears.length, 0, "reload preserves the hosting declaration");
  assert.equal(adapter.sessionCloses.length, 0, "reload preserves AgentSession");
});

test("hosting identity never falls back to selected control target", async () => {
  for (const hostTerminal of [undefined, "", "all", "@999"]) {
    const timers = new FakeTimers();
    const adapter = new FakeAdapter();
    const handlers = new Map<string, (event: unknown, ctx: unknown) => unknown>();
    const pi = { on: (name: string, handler: (event: unknown, ctx: unknown) => unknown) => handlers.set(name, handler) } as unknown as ExtensionAPI;
    const pane = projection(null).agents[0] as AgentPane;
    const store = new PhuxTargetStore({ appendEntry: () => {} }, { agentList: async () => ({ agents: [pane] }) });
    await store.refresh();
    store.select(pane);
    const { lifecycle } = registerPhuxLifecycle(pi, store, {
      cli: adapter, timers, ...(hostTerminal === undefined ? {} : { hostTerminal }),
    });
    const ctx = { sessionManager: { getSessionId: () => "session-1" } };
    handlers.get("session_start")?.({ reason: "startup" }, ctx);
    timers.runAll();
    handlers.get("agent_start")?.({}, ctx);
    await lifecycle.settled();
    await handlers.get("session_shutdown")?.({ reason: "quit" }, ctx);
    assert.deepEqual(adapter.sets, []);
    assert.deepEqual(adapter.opens, []);
    assert.deepEqual(adapter.emits, []);
    assert.deepEqual(adapter.clears, []);
  }
});

test("hosting identity works with no selected control target and quits on its own pane", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const handlers = new Map<string, (event: unknown, ctx: unknown) => unknown>();
  const pi = { on: (name: string, handler: (event: unknown, ctx: unknown) => unknown) => handlers.set(name, handler) } as unknown as ExtensionAPI;
  const pane = projection(null).agents[0] as AgentPane;
  const store = new PhuxTargetStore({ appendEntry: () => {} }, { agentList: async () => ({ agents: [pane] }) });
  await store.refresh();
  const { lifecycle } = registerPhuxLifecycle(pi, store, { cli: adapter, timers, hostTerminal: "@3" });
  const ctx = { sessionManager: { getSessionId: () => "session-1" } };
  handlers.get("session_start")?.({ reason: "startup" }, ctx);
  timers.runAll();
  await lifecycle.settled();
  assert.equal(store.snapshot.selection, null);
  assert.equal(adapter.sets[0]?.target, "@3");
  await handlers.get("session_shutdown")?.({ reason: "quit" }, ctx);
  assert.deepEqual(adapter.clears, ["@3"]);
  assert.deepEqual(adapter.sessionCloses, ["@99"]);
});

test("a trust prompt becomes blocked on the AgentSession stream", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers });

  lifecycle.start("session-1", target);
  timers.runAll();
  await lifecycle.settled();
  lifecycle.emit("ask", { kind: "trust", question: "/repo" });
  await lifecycle.settled();

  const ask = adapter.emits.find((entry) => entry.type === "ask");
  assert.ok(ask, "trust maps to ask");
  assert.equal(ask.data?.kind, "trust");
  assert.equal(adapter.sets.every((entry) => entry.record.state === undefined), true);
});

test("unsupported session open fails closed and identity-only still writes no state", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  adapter.failOpen = new PhuxError("command_failed", "phux agent session open failed", {
    stderr: '{"error":{"code":"unsupported_server"}}',
  });
  const lifecycle = new PhuxLifecycle({ cli: adapter, timers });

  lifecycle.start("session-1", target);
  timers.runAll();
  await lifecycle.settled();
  lifecycle.emit("ask", { kind: "permission" });
  lifecycle.emit("prompt");
  await lifecycle.settled();

  assert.equal(adapter.opens.length, 1);
  assert.equal(adapter.emits.length, 0, "detector remains the fallback when emit is absent");
  assert.equal(adapter.sets.length, 1);
  assert.equal(adapter.sets[0]?.record.state, undefined);
});

test("transcript entries ride provider_raw beside the typed records, and transcript:false silences them", async () => {
  for (const enabled of [true, false]) {
    const timers = new FakeTimers();
    const adapter = new FakeAdapter();
    const handlers = new Map<string, (event: unknown, ctx: unknown) => unknown>();
    const pi = { on: (name: string, handler: (event: unknown, ctx: unknown) => unknown) => handlers.set(name, handler) } as unknown as ExtensionAPI;
    const pane = projection(null).agents[0] as AgentPane;
    const store = new PhuxTargetStore({ appendEntry: () => {} }, { agentList: async () => ({ agents: [pane] }) });
    await store.refresh();
    const { lifecycle } = registerPhuxLifecycle(pi, store, {
      cli: adapter, timers, hostTerminal: "3", ...(enabled ? {} : { transcript: false }),
    });
    const ctx = { sessionManager: { getSessionId: () => "session-1" } };
    handlers.get("session_start")?.({ reason: "startup" }, ctx);
    timers.runAll();
    handlers.get("agent_start")?.({}, ctx);
    handlers.get("message_end")?.({ message: { role: "user", content: "hi", timestamp: 1 } }, ctx);
    handlers.get("tool_execution_start")?.({ toolCallId: "c1", toolName: "bash", args: { command: "ls" } }, ctx);
    handlers.get("tool_execution_end")?.({ toolCallId: "c1", toolName: "bash", isError: false, result: { content: [] } }, ctx);
    handlers.get("message_end")?.({ message: { role: "assistant", content: [{ type: "text", text: "ok" }], timestamp: 2 } }, ctx);
    handlers.get("agent_settled")?.({}, ctx);
    await lifecycle.settled();

    const summary = adapter.emits.map((emit) => {
      if (emit.type !== "provider_raw") return emit.type;
      const entry = emit.data?.entry as { id: string; final: boolean };
      assert.equal(emit.data?.schema, "phux.transcript/v1");
      assert.equal(emit.data?.provider, "pi");
      return `raw:${entry.id}:${entry.final ? "final" : "partial"}`;
    });
    assert.deepEqual(summary, enabled
      ? ["session_start", "prompt", "raw:user-1:final", "tool_start", "raw:c1:partial", "tool_end",
        "raw:c1:final", "raw:assistant-2:final", "stop"]
      : ["session_start", "prompt", "tool_start", "tool_end", "stop"]);
  }
});
