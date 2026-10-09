import { describe, test, expect } from "bun:test";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { OmpLifecycle, registerOmpLifecycle } from "../src/lifecycle.js";
import type { AgentPane, AgentRecord } from "../../runtime/src/schemas.js";

function fixture() {
  let next = 90;
  const calls: Array<{ verb: string; target?: string; data?: unknown }> = [];
  const pane: AgentPane = {
    terminal: "@17", session: "smoke", window: "window-0", agent_session: null,
    agent: { id: "omp", kind: "omp", label: "OMP" }, state: "idle", confidence: 1,
    attention: "none", title: null, cwd: null, sources: [], explanation: "fixture",
  };
  const mutable = pane as any;
  const cli = {
    async agentShow() { calls.push({ verb: "show" }); return { schema_version: 1 as const, agents: [pane] }; },
    async agentSet(target: string, record: AgentRecord) {
      calls.push({ verb: "set", target, data: record });
      mutable.sources = [{ kind: "agent_record", signal: "declared", confidence: 1, observed: JSON.stringify(record) }];
      return record;
    },
    async agentClear(target: string) { calls.push({ verb: "clear", target }); mutable.sources = []; },
    async agentSessionOpen(target: string, options: any) {
      calls.push({ verb: "open", target });
      const child = { resource: `@${++next}`, provider: options.provider, native_id: options.nativeId };
      mutable.agent_session = child;
      return { schema_version: 1 as const, parent: target, ...child };
    },
    async agentEmit(target: string, type: any, options?: any) {
      calls.push({ verb: type, target, data: options?.data });
      return { schema_version: 1 as const, resource: target, seq: 1 } as any;
    },
    async agentSessionClose(target: string) {
      calls.push({ verb: "close", target });
      if (mutable.agent_session?.resource === target) mutable.agent_session = null;
      return { resource: target } as any;
    },
  };
  return { cli, pane: mutable, calls, mutations: () => calls.filter(call => call.verb !== "show") };
}

function sdk(cli: ReturnType<typeof fixture>["cli"]) {
  const handlers = new Map<string, Array<(event: any, ctx: ExtensionContext) => unknown>>();
  let id = "one";
  let continuing = false;
  let idle = true;
  const api = { on(name: string, handler: any) { handlers.set(name, [...handlers.get(name) ?? [], handler]); } };
  registerOmpLifecycle(api as ExtensionAPI, cli, "@17");
  const ctx = { sessionManager: { getSessionId: () => id }, isIdle: () => idle } as ExtensionContext;
  return {
    setId(value: string) { id = value; },
    /** Whether OMP reports the session streaming (ctx.isIdle() false). */
    setStreaming(value: boolean) { idle = !value; },
    async event(type: string, fields: Record<string, unknown> = {}) {
      if (type === "agent_start" && !continuing) {
        for (const handler of handlers.get("before_agent_start") ?? []) await handler({ type: "before_agent_start" }, ctx);
      }
      if (type === "agent_start") continuing = false;
      if (type === "agent_end") continuing = fields.willContinue === true;
      for (const handler of handlers.get(type) ?? []) await handler({ type, ...fields }, ctx);
    },
  };
}

describe("OMP host lifecycle", () => {
  test("identity-only declaration and private native event mapping, continuation never settles", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start");
    await host.event("agent_start", { prompt: "secret" });
    await host.event("tool_execution_start", { toolName: "bash", toolCallId: "a", args: { secret: true } });
    await host.event("tool_execution_end", { toolName: "bash", toolCallId: "a", isError: true, result: "secret" });
    await host.event("agent_end", { willContinue: true });
    await host.event("turn_end"); await host.event("session_stop");
    expect(f.calls.some(call => call.verb === "stop")).toBe(false);
    await host.event("agent_end"); await host.event("session_shutdown");
    expect(f.mutations().map(call => call.verb)).toEqual(["set", "open", "session_start", "prompt", "tool_start", "tool_end", "stop", "session_end", "close", "clear"]);
    expect(f.mutations()[0]!.data).toEqual({ name: "omp", kind: "omp", session: "omp:one" });
    expect(JSON.stringify(f.calls)).not.toContain("secret");
    expect(f.calls.find(call => call.verb === "tool_end")!.data).toEqual({ tool_name: "bash", tool_use_id: "a", ok: false });
    expect(f.calls.filter(call => ["prompt", "stop", "close"].includes(call.verb)).every(call => call.target === "@91")).toBe(true);
  });

  test("reload/tree idempotence, switch/branch rotation, late old loop discarded", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("session_switch"); await host.event("session_tree");
    expect(f.calls.filter(call => call.verb === "open")).toHaveLength(1);
    await host.event("agent_start");
    await host.event("tool_execution_start", { toolName: "bash", toolCallId: "old" });
    host.setId("two"); await host.event("session_branch");
    await host.event("agent_end");
    await host.event("agent_start");
    await host.event("tool_execution_end", { toolName: "bash", toolCallId: "old", isError: false });
    expect(f.calls.filter(call => call.verb === "tool_end")).toHaveLength(0);
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(0);
    host.setId("three"); await host.event("session_switch"); await host.event("session_shutdown");
    expect(f.calls.filter(call => call.verb === "close").map(call => call.target)).toEqual(["@91"]);
  });

  for (const [name, alter] of Object.entries({
    "missing child field": (p: any) => delete p.agent_session,
    "foreign terminal": (p: any) => p.terminal = "@18",
    "missing session": (p: any) => p.session = "",
    "missing window": (p: any) => p.window = "",
    "foreign declaration": (p: any) => p.sources = [{ kind: "agent_record", observed: '{"name":"pi"}' }],
    "malformed declaration": (p: any) => p.sources = [{ kind: "agent_record", observed: 'null' }],
    "unowned child": (p: any) => p.agent_session = { resource: "@91", provider: "omp", native_id: "one" },
  })) test(`no mutation: ${name}`, async () => {
    const f = fixture(); alter(f.pane); const lifecycle = new OmpLifecycle(f.cli, "@17");
    await lifecycle.navigate("one"); await lifecycle.navigate("two"); await lifecycle.shutdown();
    expect(f.mutations()).toEqual([]);
  });

  for (const child of ["@17", "bad", "@91"]) test(`adoption child validation ${child}`, async () => {
    const f = fixture(); await f.cli.agentSet("@17", { name: "omp", kind: "omp", session: "omp:one" }); f.calls.length = 0;
    f.pane.agent_session = { resource: child, provider: child === "@91" ? "pi" : "omp", native_id: "one" };
    await new OmpLifecycle(f.cli, "@17").navigate("one"); expect(f.mutations()).toEqual([]);
  });

  test("adopt exact child; replacement fences emits and close and declaration clear", async () => {
    const f = fixture(); await f.cli.agentSet("@17", { name: "omp", kind: "omp", session: "omp:one" });
    f.pane.agent_session = { resource: "@99", provider: "omp", native_id: "one" }; f.calls.length = 0;
    const lifecycle = new OmpLifecycle(f.cli, "@17"); await lifecycle.navigate("one");
    expect(f.mutations()).toEqual([]);
    f.pane.agent_session = { resource: "@100", provider: "omp", native_id: "one" };
    const token = lifecycle.token()!; await lifecycle.emit(token.id, token.generation, "prompt"); await lifecycle.shutdown();
    expect(f.mutations()).toEqual([]);
  });

  test("active same-ID reload retires child even without an abort completion", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("agent_start");
    await host.event("tool_execution_start", { toolName: "bash", toolCallId: "live" });
    await host.event("session_switch"); await host.event("session_tree");
    await host.event("tool_execution_end", { toolName: "bash", toolCallId: "live", isError: false });
    await host.event("agent_end");
    expect(f.calls.filter(call => call.verb === "tool_end")).toHaveLength(0);
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(0);
    expect(f.pane.agent_session).toBeNull();
    expect(f.calls.filter(call => call.verb === "open")).toHaveLength(1);
  });

  test("foreign owner appearing after declaration fences open", async () => {
    const f = fixture(); const set = f.cli.agentSet;
    f.cli.agentSet = async (target, record) => {
      const result = await set(target, record);
      f.pane.sources = [{ kind: "agent_record", observed: '{"name":"pi"}' }];
      return result;
    };
    await new OmpLifecycle(f.cli, "@17").navigate("one");
    expect(f.mutations().map(call => call.verb)).toEqual(["set"]);
  });

  test("ambiguous branch during old loop cannot stop a new loop", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("agent_start");
    host.setId("two"); await host.event("session_branch");
    await host.event("agent_start"); await host.event("agent_end");
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(0);
    expect(f.calls.filter(call => call.verb === "prompt")).toHaveLength(1);
    expect(f.calls.filter(call => call.verb === "open")).toHaveLength(1);
  });

  test("concurrent approvals stay blocked; receipts are native-ID and tool-name fenced", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("agent_start");
    const approval = { sessionId: "one", toolCallId: "a", toolName: "bash", reason: "secret", approved: false };
    await host.event("tool_approval_requested", { ...approval, sessionId: "foreign" });
    await host.event("tool_approval_requested", approval);
    await host.event("tool_approval_requested", approval);
    await host.event("tool_approval_requested", { ...approval, toolCallId: "b" });
    const mark = f.calls.length;
    await host.event("tool_execution_start", { toolCallId: "other", toolName: "bash" });
    await host.event("tool_approval_resolved", { ...approval, toolName: "wrong" });
    await host.event("tool_approval_resolved", approval);
    expect(f.calls.slice(mark).filter(call => ["prompt", "tool_start", "state"].includes(call.verb))).toHaveLength(0);
    await host.event("tool_approval_resolved", { ...approval, toolCallId: "b" });
    await host.event("tool_approval_resolved", approval);
    await host.event("tool_approval_requested", approval);
    expect(f.calls.filter(call => call.verb === "notification").map(call => call.data)).toEqual([{ kind: "permission" }, { kind: "permission" }]);
    expect(f.calls.filter(call => call.verb === "state").map(call => call.data)).toEqual([{ state: "working" }]);
    expect(JSON.stringify(f.calls)).not.toContain("secret");
  });

  test("successive normal prompts and delivered automatic continuation remain authoritative", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("agent_start");
    await host.event("agent_end", { willContinue: true }); await host.event("agent_start");
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(0);
    await host.event("agent_end"); await host.event("agent_start"); await host.event("agent_end");
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(2);
    expect(f.calls.filter(call => call.verb === "prompt")).toHaveLength(3);
    expect(f.calls.filter(call => call.verb === "close")).toHaveLength(0);
  });

  test("a tool delivery held past the run's end cannot resurrect the finished run", async () => {
    // 18.8.6 settles agent_end on its own path: an earlier extension that holds
    // tool_execution_start lets the end, and even the tool's own end, overtake it.
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("agent_start");
    await host.event("tool_execution_end", { toolName: "inert", toolCallId: "late", isError: false });
    await host.event("agent_end");
    await host.event("tool_execution_start", { toolName: "inert", toolCallId: "late" });
    await host.event("tool_execution_end", { toolName: "inert", toolCallId: "late", isError: false });
    const verbs = f.mutations().map(call => call.verb);
    expect(verbs.slice(verbs.indexOf("stop"))).toEqual(["stop"]);
    expect(f.calls.filter(call => call.verb === "close")).toHaveLength(0);
  });

  test("continuation using awaited before hook does not fall back", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("agent_start");
    await host.event("agent_end", { willContinue: true });
    await host.event("before_agent_start"); await host.event("agent_start");
    expect(f.calls.filter(call => call.verb === "close")).toHaveLength(0);
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(0);
    expect(f.calls.filter(call => call.verb === "prompt")).toHaveLength(2);
  });

  test("queued delivery absorbed by a streaming loop keeps reporting", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); host.setStreaming(true); await host.event("agent_start");
    // 18.x prepares queued steering/follow-up through the hook mid-run; the loop
    // absorbs it, so no agent_start of its own follows.
    await host.event("before_agent_start");
    await host.event("tool_execution_start", { toolCallId: "q", toolName: "bash" });
    await host.event("tool_execution_end", { toolCallId: "q", toolName: "bash", isError: false });
    host.setStreaming(false); await host.event("agent_end");
    expect(f.calls.filter(call => call.verb === "close")).toHaveLength(0);
    expect(f.calls.filter(call => call.verb === "prompt")).toHaveLength(1);
    expect(f.calls.filter(call => call.verb === "tool_end")).toHaveLength(1);
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(1);
    expect(f.pane.agent_session).not.toBeNull();
    await host.event("agent_start"); await host.event("agent_end");
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(2);
  });

  test("a repeated guard while one prompt prepares is idempotent", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); host.setStreaming(true);
    await host.event("before_agent_start"); await host.event("agent_start");
    expect(f.calls.filter(call => call.verb === "close")).toHaveLength(0);
    expect(f.calls.filter(call => call.verb === "prompt")).toHaveLength(1);
  });

  test("a streaming guard followed by its own start is still an overlap", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); host.setStreaming(true); await host.event("agent_start");
    await host.event("agent_start"); await host.event("agent_end");
    expect(f.pane.agent_session).toBeNull();
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(0);
  });

  test("a guard while the session reports idle mid-loop falls back", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("agent_start");
    await host.event("before_agent_start");
    expect(f.pane.agent_session).toBeNull();
  });

  test("fallback before startup closes even an adoptable existing child", async () => {
    const f = fixture(); const lifecycle = new OmpLifecycle(f.cli, "@17");
    await lifecycle.fallback();
    await f.cli.agentSet("@17", { name: "omp", kind: "omp", session: "omp:one" });
    f.pane.agent_session = { resource: "@99", provider: "omp", native_id: "one" };
    await lifecycle.navigate("one");
    expect(f.pane.agent_session).toBeNull();
    expect(f.calls.filter(call => call.verb === "open")).toHaveLength(0);
  });

  test("overlapping starts without navigation retire the child", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("agent_start"); await host.event("agent_start");
    await host.event("agent_end");
    expect(f.pane.agent_session).toBeNull();
    expect(f.calls.filter(call => call.verb === "stop")).toHaveLength(0);
  });

  test("prepared prompt cancelled before producer start falls back on navigation", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start"); await host.event("before_agent_start");
    await host.event("session_switch", { reason: "resume" });
    expect(f.pane.agent_session).toBeNull();
    expect(f.calls.filter(call => call.verb === "prompt")).toHaveLength(0);
  });

  for (const state of [undefined, null, "unknown", "idle", "working", "blocked", "done"]) {
    test(`cold bare OMP identity initializes without declaring observed state: ${state}`, async () => {
      const f = fixture(); const host = sdk(f.cli);
      f.pane.sources = [{ kind: "agent_record", observed: JSON.stringify({ name: "omp", kind: "omp", state }) }];
      await host.event("session_start");
      expect(f.mutations()[0]).toEqual({ verb: "set", target: "@17", data: { name: "omp", kind: "omp", session: "omp:one" } });
      expect(f.pane.agent_session?.native_id).toBe("one");
      expect(f.calls.filter(call => call.verb === "open")).toHaveLength(1);
    });
  }

  test("cold null owner/attention and observational state are replaced, never preserved", async () => {
    const f = fixture(); const host = sdk(f.cli);
    f.pane.sources = [{ kind: "agent_record", observed: JSON.stringify({ name: "omp", kind: "omp", session: null, attention: null, state: "blocked" }) }];
    await host.event("session_start");
    expect(f.mutations()[0]!.data).toEqual({ name: "omp", kind: "omp", session: "omp:one" });
    expect(f.pane.agent_session?.provider).toBe("omp");
  });

  for (const [name, record] of Object.entries({
    "foreign owner": { session: "pi:other" }, "empty owner": { session: "" },
    "numeric owner": { session: 0 }, "false owner": { session: false },
    "foreign name": { name: "pi" }, "foreign kind": { kind: "pi" },
    "attention none": { attention: "none" }, "attention high": { attention: "high" },
    "attention false": { attention: false }, "unknown field": { custom: true },
    "noncanonical state": { state: "busy" }, "empty state": { state: "" },
    "structured state": { state: {} }, "numeric state": { state: 0 },
  })) test(`cold unbound refusal: ${name}`, async () => {
    const f = fixture(); const host = sdk(f.cli);
    f.pane.sources = [{ kind: "agent_record", observed: JSON.stringify({ name: "omp", kind: "omp", ...record }) }];
    await host.event("session_start"); expect(f.mutations()).toEqual([]);
  });

  for (const observed of ['null', '[]', '"omp"', '{', '{"name":"omp","kind":"omp","session":"foreign","session":null}']) {
    test(`cold malformed or duplicate JSON refusal: ${observed}`, async () => {
      const f = fixture(); const host = sdk(f.cli);
      f.pane.sources = [{ kind: "agent_record", observed }];
      await host.event("session_start"); expect(f.mutations()).toEqual([]);
    });
  }

  for (const invalid of ["duplicate sources", "existing child", "missing projection"] as const) {
    test(`cold unbound refusal: ${invalid}`, async () => {
      const f = fixture(); const host = sdk(f.cli);
      const source = { kind: "agent_record", observed: '{"name":"omp","kind":"omp"}' };
      f.pane.sources = invalid === "duplicate sources" ? [source, source] : [source];
      if (invalid === "existing child") f.pane.agent_session = { resource: "@99", provider: "omp", native_id: "one" };
      if (invalid === "missing projection") delete f.pane.agent_session;
      await host.event("session_start"); expect(f.mutations()).toEqual([]);
    });
  }

  test("bare identity does not authorize warm navigation or recovery", async () => {
    const f = fixture(); const host = sdk(f.cli);
    await host.event("session_start");
    f.calls.length = 0;
    f.pane.sources = [{ kind: "agent_record", observed: '{"name":"omp","kind":"omp","state":"idle"}' }];
    f.pane.agent_session = null;
    host.setId("two"); await host.event("session_switch"); await host.event("session_start");
    expect(f.mutations()).toEqual([]);
  });

  test("first event being navigation consumes cold bootstrap eligibility", async () => {
    const f = fixture(); const host = sdk(f.cli);
    f.pane.sources = [{ kind: "agent_record", observed: '{"name":"omp","kind":"omp"}' }];
    await host.event("session_switch"); await host.event("session_start");
    expect(f.mutations()).toEqual([]);
  });

  test("completed guarded loops can reuse approval IDs, including continuation", async () => {
    const f = fixture(); const host = sdk(f.cli);
    const receipt = { sessionId: "one", toolCallId: "reused", toolName: "bash", approved: false };
    await host.event("session_start");
    for (const willContinue of [true, false, false]) {
      await host.event("agent_start");
      await host.event("tool_approval_requested", receipt);
      await host.event("tool_approval_requested", receipt);
      await host.event("tool_approval_resolved", receipt);
      await host.event("tool_approval_resolved", receipt);
      await host.event("agent_end", { willContinue });
    }
    expect(f.calls.filter(call => call.verb === "notification")).toHaveLength(3);
    expect(f.calls.filter(call => call.verb === "state")).toHaveLength(3);
    expect(f.calls.filter(call => call.verb === "close")).toHaveLength(0);
  });

  test("outside phux does nothing", async () => {
    const f = fixture(); const lifecycle = new OmpLifecycle(f.cli); await lifecycle.navigate("one"); await lifecycle.shutdown(); expect(f.calls).toEqual([]);
  });

  test("stale queued binding never mutates, IDs captured before awaits", async () => {
    const f = fixture(); const lifecycle = new OmpLifecycle(f.cli, "@17");
    const old = lifecycle.navigate("one"); const next = lifecycle.navigate("two"); await Promise.all([old, next]);
    expect(f.mutations()[0]!.data).toEqual({ name: "omp", kind: "omp", session: "omp:two" });
  });

  for (const verb of ["agentSet", "agentSessionOpen", "agentEmit", "agentSessionClose"] as const) {
    test(`uncertain ${verb} failure is never retried`, async () => {
      const f = fixture(); let attempts = 0;
      f.cli[verb] = (async () => { ++attempts; throw new Error("unsupported_server or uncertain mutation"); }) as any;
      const lifecycle = new OmpLifecycle(f.cli, "@17"); await lifecycle.navigate("one"); await lifecycle.navigate("two"); await lifecycle.navigate("three"); await lifecycle.shutdown();
      expect(attempts).toBe(1);
    });
  }

  test("hung CLI ignoring AbortSignal still bounds shutdown and cannot mutate later", async () => {
    const f = fixture(); f.cli.agentShow = () => new Promise(() => {});
    const lifecycle = new OmpLifecycle(f.cli, "@17", 5_000);
    void lifecycle.navigate("one"); await Promise.resolve(); await Promise.resolve();
    const start = performance.now(); await lifecycle.shutdown();
    expect(performance.now() - start).toBeLessThan(1_600);
    expect(f.mutations()).toEqual([]);
  });
});
