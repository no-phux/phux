import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

import {
  PhuxLifecycle,
  registerPhuxLifecycle,
  PhuxLifecycleShutdownError,
  type LifecycleCommandOptions,
  type LifecycleTimers,
  type PhuxLifecycleAdapter,
} from "../src/lifecycle.js";
import type { AgentPane, AgentRecord, AgentStateList } from "../src/schemas.js";
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
  readonly commandOptions: LifecycleCommandOptions[] = [];
  record: AgentRecord | null = null;
  failSet = false;

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
    return projection(this.record);
  }

  async agentClear(selector: string, options: LifecycleCommandOptions): Promise<void> {
    this.commandOptions.push(options);
    this.clears.push(selector);
    this.record = null;
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

  assert.equal(adapter.commandOptions.length, 3);
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

/**
 * Previously "maps agent_start to working and agent_settled to idle". It no
 * longer subscribes to either: the server derives state from `rules/pi.toml`,
 * and a per-turn write would clobber that derivation (phux-w7z2.38, .37).
 * `agent_end` was never subscribed and still is not.
 */
test("registration subscribes to no per-turn lifecycle event", async () => {
  const timers = new FakeTimers();
  const adapter = new FakeAdapter();
  const handlers = new Map<string, (event: unknown, ctx: unknown) => unknown>();
  const pi = {
    on: (name: string, handler: (event: unknown, ctx: unknown) => unknown) => handlers.set(name, handler),
  } as unknown as ExtensionAPI;
  const pane = projection(null).agents[0] as AgentPane;
  const store = new PhuxTargetStore({ appendEntry: () => {} }, { agentList: async () => ({ agents: [pane] }) });
  store.select(pane);
  const registered = registerPhuxLifecycle(pi, store, { cli: adapter, timers });
  const ctx = {
    sessionManager: { getSessionId: () => "session-1" },
  } as unknown as ExtensionContext;

  handlers.get("session_start")?.({ type: "session_start", reason: "startup" }, ctx);
  timers.runAll();
  await flush();
  const afterStart = adapter.sets.length;
  assert.equal(afterStart, 1, "session start declares identity once");
  assert.equal(adapter.sets.at(-1)?.record.state, undefined);

  for (const event of ["agent_start", "agent_settled", "agent_end"]) {
    assert.equal(
      handlers.has(event),
      false,
      `${event} must not be subscribed: a per-turn write clobbers the derived state`,
    );
  }

  timers.runAll();
  await flush();
  assert.equal(adapter.sets.length, afterStart, "no turn produced another write");

  const shutdown = handlers.get("session_shutdown")?.(
    { type: "session_shutdown", reason: "reload" },
    ctx,
  );
  await Promise.resolve(shutdown);
  const writesAtShutdown = adapter.sets.length;
  store.select({ ...pane, terminal: "@4", session: "other", window: "window-1" });
  timers.runAll();
  await registered.lifecycle.settled();
  assert.equal(adapter.sets.length, writesAtShutdown, "shutdown unsubscribes from target changes");
});
