import { afterEach, expect, mock, spyOn, test } from "bun:test";
import type { Plugin } from "@opencode/plugin";
import type { Info, ToolContext } from "@opencode/plugin/promise/tool";

import plugin from "../src/index.js";
import { PhuxCli } from "../../runtime/src/adapter.js";

const savedParent = process.env.PHUX_TERMINAL_ID;
const savedTarget = process.env.PHUX_TARGET;
afterEach(() => {
  mock.restore();
  restoreEnvironment("PHUX_TERMINAL_ID", savedParent);
  restoreEnvironment("PHUX_TARGET", savedTarget);
});

test("selection is session-local, deletion forgets it, and sibling creation never moves identity", async () => {
  process.env.PHUX_TERMINAL_ID = "7";
  process.env.PHUX_TARGET = "@9";
  const calls = fakeCli();
  const host = await setupHost();
  try {
    await host.execute("phux_create", { name: "worker-a" }, "a");
    await host.execute("phux_snapshot", {}, "a");
    await host.execute("phux_snapshot", {}, "b");
    await host.execute("phux_create", { name: "worker-b" }, "b");
    await host.execute("phux_snapshot", {}, "a");
    await host.execute("phux_snapshot", {}, "b");
    expect(calls.snapshots).toEqual(["@10", "@9", "@10", "@11"]);

    await host.before({ sessionID: "a", tool: "phux_snapshot", id: "call" });
    await host.before({ sessionID: "b", tool: "phux_snapshot", id: "call" });
    expect(calls.identities).toEqual([["@7", "opencode:a"]]);
    expect(calls.sessionWrites.length).toBeGreaterThan(0);
    expect(calls.sessionWrites.every((target) => target === "@70")).toBe(true);
    await host.event({ type: "session.deleted", data: { sessionID: "a" } });
    await host.execute("phux_snapshot", {}, "a");
    await host.execute("phux_snapshot", {}, "b");
    expect(calls.snapshots.slice(-2)).toEqual(["@9", "@11"]);
    await host.before({ sessionID: "b", tool: "phux_snapshot", id: "next" });
    expect(calls.identities).toEqual([["@7", "opencode:a"], ["@7", "opencode:b"]]);
  } finally {
    await host.close();
  }
});

test("standalone explicit target remains identity when another target is selected", async () => {
  delete process.env.PHUX_TERMINAL_ID;
  process.env.PHUX_TARGET = "host/@9";
  const calls = fakeCli();
  const host = await setupHost();
  try {
    await host.execute("phux_create", { name: "worker" }, "a");
    await host.before({ sessionID: "a", tool: "phux_snapshot", id: "call" });
    expect(calls.identities).toEqual([["host/@9", "opencode:a"]]);
    await host.execute("phux_snapshot", {}, "a");
    expect(calls.snapshots).toEqual(["@10"]);
  } finally {
    await host.close();
  }
});

test("host cancellation interrupts an in-flight CLI operation", async () => {
  delete process.env.PHUX_TERMINAL_ID;
  delete process.env.PHUX_TARGET;
  const { promise: ready, resolve: started } = Promise.withResolvers<void>();
  spyOn(PhuxCli.prototype, "wait").mockImplementation(async (options) => {
    started();
    const { promise, reject } = Promise.withResolvers<never>();
    options.signal?.addEventListener("abort", () => reject(new Error("host cancelled")), { once: true });
    return promise;
  });
  const host = await setupHost();
  try {
    const controller = new AbortController();
    const pending = host.execute("phux_wait", { target: "@8", until: "ready" }, "a", controller.signal);
    await ready;
    controller.abort();
    await expect(pending).rejects.toThrow("host cancelled");
  } finally {
    await host.close();
  }
});

test("shared service does not claim a pane without explicit server lifecycle opt-in", async () => {
  process.env.PHUX_TERMINAL_ID = "7";
  const calls = fakeCli();
  const host = await setupHost(false);
  try {
    await host.event({ type: "session.status", data: { sessionID: "a", status: { type: "busy" } } });
    expect(calls.identities).toEqual([]);
    expect(calls.sessionWrites).toEqual([]);
  } finally {
    await host.close();
  }
});

function fakeCli() {
  let next = 10;
  const snapshots: string[] = [];
  const identities: unknown[][] = [];
  const sessionWrites: string[] = [];
  spyOn(PhuxCli.prototype, "create").mockImplementation(async (name) => ({ session: name, terminal_id: next++ }));
  spyOn(PhuxCli.prototype, "snapshot").mockImplementation(async (options) => {
    snapshots.push(options.target!);
    return { pane: 7, cols: 80, rows: 24, lines: [], scrollback: [], cursor: { x: 0, y: 0, visible: true } };
  });
  spyOn(PhuxCli.prototype, "agentSet").mockImplementation(async (target, record) => {
    identities.push([target, record.session]);
    return record;
  });
  spyOn(PhuxCli.prototype, "agentShow").mockResolvedValue({ schema_version: 1, agents: [] });
  spyOn(PhuxCli.prototype, "agentSessionOpen").mockImplementation(async (target, options) => ({
    schema_version: 1, resource: "@70", parent: target, provider: options.provider, native_id: options.nativeId ?? null,
  }));
  spyOn(PhuxCli.prototype, "agentEmit").mockImplementation(async (target, type) => {
    sessionWrites.push(target);
    return { schema_version: 1, resource: target, type, seq: 1, ts_ms: 0 };
  });
  spyOn(PhuxCli.prototype, "agentSessionClose").mockImplementation(async (target) => {
    sessionWrites.push(target);
    return { resource: target, closed: true };
  });
  return { snapshots, identities, sessionWrites };
}

async function setupHost(serverLifecycle = true) {
  const tools: Record<string, Info> = {};
  const hooks: Record<string, (event: unknown) => Promise<void>> = {};
  let deliver!: (entry: { event: unknown; done: () => void } | undefined) => void;
  const context = {
    options: { contextAwareness: false, serverLifecycle },
    tool: {
      transform: async (transform: (editor: unknown) => void) => transform({ add: (tool: Info) => { tools[tool.name] = tool; } }),
      hook: async (name: string, hook: (event: unknown) => Promise<void>) => { hooks[name] = hook; },
    },
    session: { hook: async () => {} },
    event: {
      async *subscribe({ signal }: { signal: AbortSignal }) {
        while (!signal.aborted) {
          const { promise, resolve } = Promise.withResolvers<{ event: unknown; done: () => void } | undefined>();
          deliver = resolve;
          const abort = () => resolve(undefined);
          signal.addEventListener("abort", abort, { once: true });
          const entry = await promise;
          signal.removeEventListener("abort", abort);
          if (entry === undefined) return;
          yield entry.event;
          entry.done();
        }
      },
    },
  } as unknown as Plugin.Context;
  const cleanup = await plugin.setup(context);
  return {
    before: (event: unknown) => hooks["execute.before"]!(event),
    execute: (name: string, input: unknown, sessionID: string, signal = new AbortController().signal) => tools[name]!.execute(input, {
      sessionID, agent: "build", messageID: "message", id: "call", signal, progress: async () => {},
    } as ToolContext),
    event: (event: unknown) => {
      const { promise, resolve: done } = Promise.withResolvers<void>();
      deliver({ event, done });
      return promise;
    },
    close: async () => { if (cleanup !== undefined) await cleanup(); },
  };
}

function restoreEnvironment(name: string, value: string | undefined): void {
  if (value === undefined) delete process.env[name];
  else process.env[name] = value;
}
