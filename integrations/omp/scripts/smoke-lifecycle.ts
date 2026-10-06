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
    const coldPane = show();
    const coldRecords = coldPane.sources.filter((source: { kind: string }) => source.kind === "agent_record");
    assert.equal(coldRecords.length, 1);
    assert.deepEqual(JSON.parse(coldRecords[0].observed), { name: "omp", kind: "omp", state: "blocked" });
    assert.equal(coldPane.agent_session, null);
    console.log("Cold unbound OMP identity before startup (publisher is not admission evidence):", JSON.stringify(coldPane));
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
    await runner.emit({ type: "session_start" });
    const first = show().agent_session;
    assert.ok(first, JSON.stringify(show()));
    assert.equal(first.provider, "omp");
    assert.equal(first.native_id, session.sessionManager.getSessionId());
    console.log("Cold bare-record SDK startup projection:", JSON.stringify(show()));
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

    async function openCase(delayedType?: string, countedType?: string, trailingType?: string) {
      const loaded = await loadExtensions(paths, process.cwd());
      const entered = Promise.withResolvers<void>();
      const release = Promise.withResolvers<void>();
      const trailed = Promise.withResolvers<void>();
      const counted = { calls: 0 };
      let delay = true;
      // After the extension under test: the runner awaits each handler in
      // order, so this firing proves the reporter has finished with the event.
      if (trailingType) {
        const trailing = { ...loaded.extensions[0]!, tools: new Map(), commands: new Map(), handlers: new Map() };
        trailing.handlers.set(trailingType, [() => { trailed.resolve(); }]);
        loaded.extensions.push(trailing);
      }
      if (countedType) {
        const spy = { ...loaded.extensions[0]!, tools: new Map(), commands: new Map(), handlers: new Map() };
        spy.handlers.set(countedType, [() => { counted.calls++; }]);
        loaded.extensions.unshift(spy);
      }
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
      await runner.emit({ type: "session_start" });
      await session.sessionManager.ensureOnDisk();
      return { session, runner, loaded, entered, release, counted, trailed };
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
    // The wrapper reads approval settings through OMP's typed settings registry,
    // so this must be a real (isolated, in-memory) Settings instance.
    const { Settings } = await import("@oh-my-pi/pi-coding-agent/config/settings");
    const context = { sessionManager: approvalCase.session.sessionManager, settings: Settings.isolated({
      "tools.approvalMode": "always-ask", "tools.approval": { inert: "prompt" },
    }) } as any;
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

    // 17.x held agent_end behind an earlier extension's slow tool delivery with
    // a FIFO subscriber gate. 18.6.1 has no such gate: every agent event is
    // dispatched fire-and-forget, and agent_end reaches extensions from its own
    // settle path, so a held tool_execution_start lands AFTER the run's end.
    // This case pins that ordering deterministically (no sleep: the end is
    // awaited while the earlier delivery is still held) and proves the reporter
    // tolerates it: the late tool event must not resurrect the finished run.
    const delayedTool = await openCase("tool_execution_start", undefined, "tool_execution_start");
    await delayedTool.runner.emitBeforeAgentStart("", undefined, []);
    await delayedTool.runner.emit({ type: "agent_start" });
    delayedTool.session.agent.emitExternalEvent({ type: "tool_execution_start", toolCallId: "gate", toolName: "inert", args: {} });
    await delayedTool.entered.promise;
    delayedTool.session.agent.emitExternalEvent({ type: "tool_execution_end", toolCallId: "gate", toolName: "inert", result: { content: [], details: {} }, isError: false });
    delayedTool.session.agent.emitExternalEvent({ type: "agent_end", messages: [] });
    await waitUntil(() => show().state === "done");
    delayedTool.release.resolve();
    await delayedTool.trailed.promise;
    assert.equal(show().state, "done", "a tool start delivered after the end must not resurrect working");
    await delayedTool.session.waitForIdle();
    await delayedTool.runner.emitBeforeAgentStart("", undefined, []);
    await delayedTool.runner.emit({ type: "agent_start" });
    assert.equal(show().state, "working", "received aggregate permits a safe normal next loop");
    assert.ok(show().agent_session);
    await closeCase();

    const { createAssistantMessageEventStream } = await import("@oh-my-pi/pi-ai/utils/event-stream");
    // 18.x prepares a queued follow-up through before_agent_start inside the
    // running loop (AgentSession's prepareQueuedMessages), with no agent_start of
    // its own. Typing into a busy OMP must keep the exact child reporting.
    const queued = await openCase(undefined, "before_agent_start");
    // Print/RPC runtime wiring: binds ctx.isIdle() to the session's streaming
    // state (the interactive TUI binds the same). Its session_start is a same-ID
    // repeat, which navigation treats as idempotent.
    const { initializeExtensions } = await import("@oh-my-pi/pi-coding-agent/modes/runtime-init");
    await initializeExtensions(queued.session, {
      reportSendError: (_action, error) => { throw error; },
      reportRuntimeError: error => { throw new Error(String(error.error)); },
    });
    queued.session.agent.setModel({
      id: "inert", name: "inert", api: "openai-completions", provider: "fixture", identity: { class: "unknown" }, input: ["text"], reasoning: false,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 10000, maxTokens: 1000,
    } as any);
    const turns = [Promise.withResolvers<() => void>(), Promise.withResolvers<() => void>()];
    let turn = 0;
    queued.session.agent.streamFn = model => {
      const stream = createAssistantMessageEventStream();
      const message = {
        role: "assistant", content: [{ type: "text", text: "inert" }], api: model.api, provider: model.provider, model: model.id,
        usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
        stopReason: "stop", timestamp: Date.now(),
      } as any;
      turns[turn++]?.resolve(() => stream.push({ type: "done", reason: "stop", message }));
      return stream;
    };
    await queued.runner.emitBeforeAgentStart("", undefined, []);
    const queuedRun = queued.session.agent.prompt("inert local transport; never a provider call");
    const finishFirst = await turns[0]!.promise;
    await waitUntil(() => show().state === "working");
    const queuedChild = show().agent_session;
    assert.ok(queuedChild);
    const guards = queued.counted.calls;
    await queued.session.followUp("queued while busy; inert");
    finishFirst();
    const finishSecond = await turns[1]!.promise;
    assert.equal(queued.counted.calls, guards + 1, "the queued delivery ran the before hook mid-run");
    assert.equal(show().agent_session?.resource, queuedChild.resource, "absorbed queued delivery keeps the exact child");
    assert.equal(show().state, "working");
    finishSecond();
    await queuedRun;
    await queued.session.waitForIdle();
    await waitUntil(() => show().state === "done");
    assert.equal(show().agent_session?.resource, queuedChild.resource);
    await closeCase();

    // Real same-file switch disconnects listeners before abort; provide NO end.
    const reload = await openCase();
    await reload.runner.emitBeforeAgentStart("", undefined, []);
    reload.session.agent.setModel({
      id: "inert", name: "inert", api: "openai-completions", provider: "fixture", identity: { class: "unknown" }, input: ["text"], reasoning: false,
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
    const nativeRoot = join(process.cwd(), "native");
    await mkdir(nativeRoot);
    const proofPath = join(nativeRoot, "startup.json");
    const callsPath = join(nativeRoot, "provider-called");
    const providerFixture = join(nativeRoot, "provider.js");
    await writeFile(providerFixture, `export default function(api) {
      api.registerProvider("phux-smoke", {
        baseUrl: "http://127.0.0.1:1", apiKey: "inert-local-fixture-not-a-credential", api: "phux-smoke-inert",
        streamSimple() { require("node:fs").writeFileSync(${JSON.stringify(callsPath)}, "unexpected"); throw new Error("No provider calls allowed"); },
        models: [{ id: "inert", name: "Inert startup fixture", reasoning: false, input: ["text"],
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 10000, maxTokens: 1000 }]
      });
      api.on("session_start", async (_event, ctx) => {
        await Bun.write(${JSON.stringify(proofPath)}, JSON.stringify({
          nativeId: ctx.sessionManager.getSessionId(), pid: process.pid, host: process.env.PHUX_TERMINAL_ID,
          selected: process.env.PHUX_TARGET, argv: process.argv, sdk: "18.6.1"
        }));
      });
    }`);
    const stdoutPath = join(nativeRoot, "stdout.log");
    const stderrPath = join(nativeRoot, "stderr.log");
    const exitPath = join(nativeRoot, "exit.json");
    const argv = [process.execPath, nativeExecutable, "--mode", "rpc", "--no-tools", "--model", "phux-smoke/inert", "-e", providerFixture, "-e", process.argv[4]!];
    // Diagnostic wrapper preserves the CLI's real argv and captures exit/output;
    // it neither emits SDK events nor declares any agent identity.
    const launcher = join(nativeRoot, "omp.js");
    await writeFile(launcher, `const child = Bun.spawn(${JSON.stringify(argv)}, {
      stdin: "inherit", stdout: "pipe", stderr: "pipe", env: process.env
    });
    async function capture(stream, path, terminal) {
      const writer = Bun.file(path).writer();
      for await (const chunk of stream) { terminal.write(chunk); writer.write(chunk); await writer.flush(); }
      await writer.end();
    }
    const out = capture(child.stdout, ${JSON.stringify(stdoutPath)}, process.stdout);
    const err = capture(child.stderr, ${JSON.stringify(stderrPath)}, process.stderr);
    const code = await child.exited; await Promise.all([out, err]);
    await Bun.write(${JSON.stringify(exitPath)}, JSON.stringify({ code }));`);
    const nativeTarget = `@${JSON.parse(run("new", "--json", "-s", "omp-native-unassisted", "-e", `PHUX_TARGET=${sibling}`, "--", process.execPath, launcher)).terminal_id}`;
    for (let attempt = 0; attempt < 200; attempt++) {
      if (await Bun.file(proofPath).exists() || await Bun.file(exitPath).exists()) break;
      await Bun.sleep(50);
    }
    const stdout = await Bun.file(stdoutPath).text();
    const stderr = await Bun.file(stderrPath).text();
    console.log("Locked CLI argv:", JSON.stringify(argv));
    console.log("Locked CLI stdout:", stdout);
    console.log("Locked CLI stderr:", stderr);
    if (await Bun.file(exitPath).exists()) console.log("Locked CLI exit:", await Bun.file(exitPath).text());
    assert.ok(await Bun.file(proofPath).exists(), "real CLI did not deliver session_start; inspect captured exit/output");
    const proof = await Bun.file(proofPath).json();
    await waitUntil(() => show(nativeTarget).agent_session !== null);
    const nativePane = show(nativeTarget);
    assert.equal(nativePane.agent_session.native_id, proof.nativeId);
    assert.equal(nativePane.agent_session.provider, "omp");
    assert.notEqual(nativePane.agent_session.resource, nativeTarget);
    assert.equal(`@${String(proof.host).replace(/^@/, "")}`, nativeTarget);
    assert.equal(proof.selected, sibling);
    run("paste", "--", nativeTarget, JSON.stringify({ type: "get_session_stats", id: "startup-proof" }));
    run("send-keys", nativeTarget, "Enter");
    let rpcOutput = "";
    for (let attempt = 0; attempt < 100; attempt++) {
      rpcOutput = await Bun.file(stdoutPath).text();
      if (rpcOutput.includes('"startup-proof"')) break;
      await Bun.sleep(25);
    }
    const rpcState = rpcOutput.split("\n").filter(Boolean).map(line => JSON.parse(line))
      .find(value => value.id === "startup-proof");
    assert.ok(rpcState?.success, rpcOutput);
    assert.equal(rpcState.data.sessionId, proof.nativeId);
    assert.equal(rpcState.data.userMessages, 0);
    assert.equal(rpcState.data.assistantMessages, 0);
    assert.equal(rpcState.data.toolCalls, 0);
    assert.equal(await Bun.file(exitPath).exists(), false, "CLI must still be running, not a returned shell");
    process.kill(proof.pid, 0);
    console.log("Live CLI get_session_stats response:", JSON.stringify(rpcState));
    console.log("Live CLI pane snapshot:", run("snapshot", "--json", nativeTarget));
    assert.equal(await Bun.file(callsPath).exists(), false, "startup must not invoke even the inert provider");
    assert.deepEqual(show(sibling).agent, before.agent);
    assert.equal(show(sibling).agent_session, before.agent_session);
    console.log("Unassisted locked CLI native session_start:", JSON.stringify(proof));
    console.log("Unassisted locked CLI hosting projection:", JSON.stringify(nativePane));
    console.log("Native approval wrapper: blocked throughout concurrent dialogs, denied inert tools, resolved working; queued follow-up absorbed mid-run kept the child; detached-end overlaps new/resume/fork/successive starts and active same-ID reload: declaration-only, detector fallback restored.");
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
