import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

// Explicit scratch binary only. No PATH lookup, installation or production server.
if (process.argv[2] !== "--isolated") {
  const binary = process.argv[2];
  assert.ok(binary?.startsWith("/"), "usage: bun scripts/smoke-lifecycle.ts /absolute/path/to/scratch/phux");
  const root = await mkdtemp(join(tmpdir(), "phux-omp-life-"));
  try {
    await mkdir(join(root, "home"));
    const archive = join(root, "package.tgz");
    const packed = Bun.spawnSync([process.execPath, "pm", "pack", "--ignore-scripts", "--filename", archive], { cwd: resolve(dirname(import.meta.path), "..") });
    assert.equal(packed.exitCode, 0, packed.stderr.toString());
    assert.equal(Bun.spawnSync(["tar", "-xzf", archive, "-C", root]).exitCode, 0);
    const child = Bun.spawn([process.execPath, import.meta.path, "--isolated", binary, join(root, "package")], {
      cwd: root,
      env: {
        PATH: "/usr/bin:/bin:/usr/sbin:/sbin", HOME: join(root, "home"), SHELL: "/bin/sh",
        XDG_CONFIG_HOME: join(root, "config"), XDG_DATA_HOME: join(root, "data"),
        XDG_CACHE_HOME: join(root, "cache"), XDG_STATE_HOME: join(root, "state"), XDG_RUNTIME_DIR: join(root, "runtime"),
        PI_CODING_AGENT_DIR: join(root, "agent"), PHUX_SOCKET: join(root, "server.sock"),
        PHUX_BIN: binary, NO_COLOR: "1", PHUX_WS_ADDR: "127.0.0.1:0", PHUX_QUIC_ADDR: "127.0.0.1:0", PHUX_WT_ADDR: "127.0.0.1:0",
      }, stdout: "inherit", stderr: "inherit",
    });
    assert.equal(await child.exited, 0);
  } finally { await rm(root, { recursive: true, force: true }); }
} else {
  const binary = process.argv[3]!;
  const socket = process.env.PHUX_SOCKET!;
  const env = { ...process.env };
  const run = (...args: string[]) => {
    const result = Bun.spawnSync([binary, "--socket", socket, ...args], { env, timeout: 3_000 });
    assert.equal(result.exitCode, 0, result.stderr.toString());
    return result.stdout.toString();
  };
  const server = Bun.spawn([binary, "server", "--socket", socket, "--session", "omp-bootstrap"], { env, stdout: "ignore", stderr: "inherit" });
  let session: Awaited<ReturnType<typeof import("@oh-my-pi/pi-coding-agent")["createAgentSession"]>>["session"] | undefined;
  try {
    for (let tries = 0; ; tries++) {
      const ready = Bun.spawnSync([binary, "list", "--json", "--socket", socket], { env });
      if (ready.exitCode === 0) break;
      assert.ok(tries < 100, "private server failed to start");
      await Bun.sleep(50);
    }
    const screen = "────────────────────────────────────────────────────────────────────────────────\n\n Allow tool: inert\n\n ❯ Approve\n   Deny\n\n up/down navigate  enter select  esc cancel\n\n────────────────────────────────────────────────────────────────────────────────\n";
    const occupant = join(process.cwd(), "omp.js");
    await writeFile(occupant, `process.stdout.write(${JSON.stringify(screen)}); setInterval(() => {}, 1000);`);
    const target = `@${JSON.parse(run("new", "--json", "-s", "omp-host", "--", process.execPath, occupant)).terminal_id}`;
    const sibling = `@${JSON.parse(run("new", "--json", "-s", "omp-control")).terminal_id}`;
    const show = (pane = target) => JSON.parse(run("agent", "show", "--json", pane)).agents[0];
    const before = show(sibling);
    await waitUntil(() => show().state === "blocked");
    console.log("Unassisted detector admission evidence (no owner; intentionally not adopted):", JSON.stringify(show()));
    process.env.PHUX_TERMINAL_ID = target;
    process.env.PHUX_TARGET = sibling;
    const { createAgentSession, SessionManager } = await import("@oh-my-pi/pi-coding-agent");
    const { discoverExtensionPaths, loadExtensions } = await import("@oh-my-pi/pi-coding-agent/extensibility/extensions");
    const { injectOmpExtensionCliRoots } = await import("@oh-my-pi/pi-coding-agent/discovery/omp-extension-roots");
    injectOmpExtensionCliRoots([process.argv[4]!], process.env.HOME!, process.cwd());
    const paths = await discoverExtensionPaths([process.argv[4]!], process.cwd());
    const loaded = await loadExtensions(paths, process.cwd());
    assert.deepEqual(loaded.errors, []);
    assert.equal(loaded.extensions.length, 1);
    ({ session } = await createAgentSession({
      cwd: process.cwd(), agentDir: process.env.PI_CODING_AGENT_DIR,
      sessionManager: SessionManager.create(process.cwd(), join(process.cwd(), "sessions")),
      preloadedExtensions: loaded, disableExtensionDiscovery: true,
      enableMCP: false, enableLsp: false, enableIrc: false, skipPythonPreflight: true,
      toolNames: [], skills: [], rules: [], contextFiles: [], promptTemplates: [], slashCommands: [],
    }));
    const runner = session.extensionRunner!;
    assert.ok(runner.hasHandlers("session_start"));
    // Startup belongs to OMP's UI controller; activity here is scripted, NOT
    // evidence of automatic model-loop timing. Navigation below is real SDK.
    run("agent", "set", target, "--name", "omp", "--kind", "omp", "--session", `omp:${session.sessionManager.getSessionId()}`);
    await runner.emit({ type: "session_start" });
    const first = show().agent_session;
    assert.ok(first, JSON.stringify(show()));
    assert.equal(first.provider, "omp");
    assert.equal(first.native_id, session.sessionManager.getSessionId());
    await runner.emitBeforeAgentStart("", undefined, []);
    await runner.emit({ type: "agent_start" });
    await runner.emit({ type: "tool_execution_start", toolCallId: "smoke", toolName: "fixture", args: {} });
    await runner.emit({ type: "tool_execution_end", toolCallId: "smoke", toolName: "fixture", result: { content: [], details: {} }, isError: false });
    await runner.emit({ type: "agent_end", messages: [], willContinue: true });
    await runner.emit({ type: "agent_start" });
    await runner.emit({ type: "agent_end", messages: [] });
    await session.sessionManager.flush();
    // Native newSession automatically emits session_switch AFTER native ID rotation.
    await session.newSession();
    const second = show().agent_session;
    assert.notEqual(second.resource, first.resource);
    assert.equal(second.native_id, session.sessionManager.getSessionId());
    assert.notEqual(second.native_id, first.native_id);
    const entry = session.sessionManager.appendMessage({ role: "user", content: "no model call", timestamp: Date.now() });
    await session.sessionManager.flush();
    await session.sessionManager.ensureOnDisk();
    await session.switchSession(session.sessionManager.getSessionFile()!);
    assert.equal(show().agent_session.resource, second.resource, "native transcript reload is idempotent");
    await session.branch(entry);
    const third = show().agent_session;
    assert.notEqual(third.resource, second.resource);
    assert.equal(third.native_id, session.sessionManager.getSessionId());
    await runner.emit({ type: "session_shutdown" });
    assert.equal(show().agent_session, null);
    assert.ok(!show().sources.some((source: { observed: string }) => source.observed.includes("omp:")));
    assert.deepEqual(show(sibling).agent, before.agent);
    assert.equal(show(sibling).agent_session, before.agent_session);
    await session.dispose();
    session = undefined;

    async function openCase(delayedType?: string) {
      const loaded = await loadExtensions(paths, process.cwd());
      const entered = Promise.withResolvers<void>();
      const release = Promise.withResolvers<void>();
      let delay = true;
      if (delayedType) {
        const earlier = { ...loaded.extensions[0]!, tools: new Map(), commands: new Map(), handlers: new Map() };
        earlier.handlers.set(delayedType, [async () => {
          if (!delay) return;
          delay = false;
          entered.resolve();
          await release.promise;
        }]);
        loaded.extensions.unshift(earlier);
      }
      const created = await createAgentSession({
        cwd: process.cwd(), agentDir: process.env.PI_CODING_AGENT_DIR,
        sessionManager: SessionManager.create(process.cwd(), join(process.cwd(), "sessions")),
        preloadedExtensions: loaded, disableExtensionDiscovery: true,
        enableMCP: false, enableLsp: false, enableIrc: false, skipPythonPreflight: true,
        toolNames: [], skills: [], rules: [], contextFiles: [], promptTemplates: [], slashCommands: [],
      });
      session = created.session;
      const runner = session.extensionRunner!;
      run("agent", "set", target, "--name", "omp", "--kind", "omp", "--session", `omp:${session.sessionManager.getSessionId()}`);
      await runner.emit({ type: "session_start" });
      await session.sessionManager.ensureOnDisk();
      return { session, runner, loaded, entered, release };
    }

    async function closeCase() {
      await session!.extensionRunner!.emit({ type: "session_shutdown" });
      await session!.dispose();
      session = undefined;
    }

    // The real wrapper awaits passive approval events around a deferred UI.
    // Always deny: the inert tool body must never run; no permission is granted.
    const approvalCase = await openCase();
    const approvalRunner = approvalCase.runner;
    const dialogs: Array<(choice: string) => void> = [];
    approvalRunner.initialize(approvalCase.loaded.runtime as any, {
      getContextUsage: () => undefined, compact: async () => {}, getModel: () => undefined, isIdle: () => true, abort() {}, hasPendingMessages: () => false,
      shutdown() {}, getSystemPrompt: () => [],
    }, undefined, { ...approvalRunner.getUIContext(), select: async () => new Promise(resolve => dialogs.push(resolve)) });
    const { ExtensionToolWrapper } = await import("@oh-my-pi/pi-coding-agent/extensibility/extensions");
    let executions = 0;
    const tool = new ExtensionToolWrapper({
      name: "inert", label: "inert", description: "No action", approval: "exec",
      parameters: { type: "object", properties: {} },
      async execute() { ++executions; return { content: [], details: {} }; },
    }, approvalRunner);
    await approvalRunner.emitBeforeAgentStart("", undefined, []);
    await approvalRunner.emit({ type: "agent_start" });
    const context = { sessionManager: approvalCase.session.sessionManager, settings: {
      get: (key: string) => key === "tools.approvalMode" ? "prompt" : { inert: "prompt" },
    } } as any;
    const deniedA = tool.execute("approval-a", {}, undefined, undefined, context).catch(error => String(error));
    await waitUntil(() => dialogs.length === 1);
    assert.equal(show().state, "blocked", `wrapped approval must override working: ${JSON.stringify(show())}`);
    const deniedB = tool.execute("approval-b", {}, undefined, undefined, context).catch(error => String(error));
    await waitUntil(() => dialogs.length === 2);
    await approvalRunner.emit({ type: "tool_execution_start", toolCallId: "other", toolName: "inert", args: {} });
    assert.equal(show().state, "blocked", "tool_start cannot clear concurrent approval");
    dialogs[0]!("Deny"); await deniedA;
    assert.equal(show().state, "blocked", "one resolution cannot clear another approval");
    dialogs[1]!("Deny"); await deniedB;
    assert.equal(show().state, "working", "last resolution honestly retracts blocking");
    assert.equal(executions, 0);
    await approvalRunner.emit({ type: "agent_end", messages: [] });
    await closeCase();

    // Exercise detached SDK notification production, not just a direct end emit.
    for (const navigation of ["none", "new", "resume", "fork"] as const) {
      const current = await openCase("agent_end");
      await current.runner.emitBeforeAgentStart("", undefined, []);
      await current.runner.emit({ type: "agent_start" });
      current.session.agent.emitExternalEvent({ type: "agent_end", messages: [] });
      await current.entered.promise;
      if (navigation === "new") await current.session.newSession();
      if (navigation === "resume") await current.session.switchSession(current.session.sessionManager.getSessionFile()!);
      if (navigation === "fork") assert.ok(await current.session.fork());
      await current.runner.emitBeforeAgentStart("", undefined, []);
      await current.runner.emit({ type: "agent_start" });
      assert.equal(show().agent_session, null, `ambiguous ${navigation} must retire exact child`);
      current.release.resolve();
      await current.session.waitForIdle();
      await Bun.sleep(20);
      assert.equal(show().agent_session, null, "late end must not resurrect a child");
      assert.ok(show().sources.some((source: { observed: string }) => source.observed.includes(`omp:${current.session.sessionManager.getSessionId()}`)));
      await closeCase();
    }

    // Earlier extension can delay the start itself, not just the aggregate end.
    const delayedStart = await openCase("agent_start");
    await delayedStart.runner.emitBeforeAgentStart("", undefined, []);
    delayedStart.session.agent.emitExternalEvent({ type: "agent_start" });
    await delayedStart.entered.promise;
    await delayedStart.session.newSession();
    await delayedStart.runner.emitBeforeAgentStart("", undefined, []);
    await delayedStart.runner.emit({ type: "agent_start" });
    delayedStart.release.resolve();
    await Bun.sleep(20);
    assert.equal(show().agent_session, null, "prepared old producer is ambiguous even before start receipt");
    await closeCase();

    const delayedBefore = await openCase("before_agent_start");
    const firstGuard = delayedBefore.runner.emitBeforeAgentStart("", undefined, []);
    await delayedBefore.entered.promise;
    await delayedBefore.runner.emitBeforeAgentStart("", undefined, []);
    await delayedBefore.runner.emit({ type: "agent_start" });
    delayedBefore.release.resolve(); await firstGuard;
    assert.equal(show().agent_session, null, "late earlier before guard must not adopt the newer loop");
    await closeCase();

    // Exercise the SDK's actual FIFO subscriber gate: end is not delivered to
    // extensions while an earlier generic tool delivery is still held up.
    const delayedTool = await openCase("tool_execution_start");
    await delayedTool.runner.emitBeforeAgentStart("", undefined, []);
    await delayedTool.runner.emit({ type: "agent_start" });
    delayedTool.session.agent.emitExternalEvent({ type: "tool_execution_start", toolCallId: "gate", toolName: "inert", args: {} });
    await delayedTool.entered.promise;
    delayedTool.session.agent.emitExternalEvent({ type: "tool_execution_end", toolCallId: "gate", toolName: "inert", result: { content: [], details: {} }, isError: false });
    delayedTool.session.agent.emitExternalEvent({ type: "agent_end", messages: [] });
    await Bun.sleep(30);
    assert.equal(show().state, "working", "aggregate must wait for the earlier generic delivery gate");
    delayedTool.release.resolve();
    await delayedTool.session.waitForIdle();
    await waitUntil(() => show().state === "done");
    await delayedTool.runner.emitBeforeAgentStart("", undefined, []);
    await delayedTool.runner.emit({ type: "agent_start" });
    assert.equal(show().state, "working", "received aggregate permits a safe normal next loop");
    assert.ok(show().agent_session);
    await closeCase();

    // Real same-file switch disconnects listeners before abort; provide NO end.
    const reload = await openCase();
    await reload.runner.emitBeforeAgentStart("", undefined, []);
    const { createAssistantMessageEventStream } = await import("@oh-my-pi/pi-ai/utils/event-stream");
    reload.session.agent.setModel({
      id: "inert", name: "inert", api: "openai-completions", provider: "fixture", input: ["text"], reasoning: false,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 10000, maxTokens: 1000,
    } as any);
    const streaming = Promise.withResolvers<void>();
    reload.session.agent.streamFn = (_model, _context, options) => {
      const stream = createAssistantMessageEventStream();
      options?.signal?.addEventListener("abort", () => stream.fail(new Error("inert local transport aborted")), { once: true });
      streaming.resolve();
      return stream;
    };
    const running = reload.session.agent.prompt("inert local transport; never a provider call").catch(() => {});
    await streaming.promise;
    await waitUntil(() => show().state === "working");
    assert.equal(reload.session.agent.state.isStreaming, true);
    await reload.session.switchSession(reload.session.sessionManager.getSessionFile()!);
    await running;
    assert.equal(show().agent_session, null, "active same-ID reload must restore detector eligibility");
    await waitUntil(() => show().state === "blocked");
    assert.equal(show().agent_session, null);
    assert.equal(show().state, "blocked", "screen detector restored after child retirement");
    await closeCase();
    assert.deepEqual(show(sibling).agent, before.agent);
    assert.equal(show(sibling).agent_session, before.agent_session);
    const nativeExecutable = resolve(dirname(import.meta.path), "../node_modules/.bin/omp");
    const nativeTarget = `@${JSON.parse(run("new", "--json", "-s", "omp-native-unassisted", "--", process.execPath, nativeExecutable, "--mode", "rpc", "--no-session", "--no-tools", "-e", process.argv[4]!)).terminal_id}`;
    await Bun.sleep(5_000);
    console.log("Unassisted locked OMP CLI startup projection:", JSON.stringify(show(nativeTarget)));
    console.log("Unassisted locked OMP CLI startup snapshot:", run("snapshot", "--json", nativeTarget));
    console.log("Native approval wrapper: blocked throughout concurrent dialogs, denied inert tools, resolved working; detached-end overlaps new/resume/fork/successive starts and active same-ID reload: declaration-only, detector fallback restored.");
    console.log("Private server + packed locked OMP SDK: identity, scripted activity, automatic new/switch/reload/branch, shutdown and untouched sibling passed; no model calls.");
  } finally {
    await session?.dispose();
    server.kill();
    await server.exited;
  }
}

async function waitUntil(predicate: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (predicate()) return;
    await Bun.sleep(25);
  }
  throw new Error("Timed out waiting for isolated lifecycle evidence");
}
