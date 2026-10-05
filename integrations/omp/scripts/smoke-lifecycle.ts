import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
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
    const result = Bun.spawnSync([binary, ...args, "--socket", socket], { env, timeout: 3_000 });
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
    const target = `@${JSON.parse(run("new", "--json", "-s", "omp-host")).terminal_id}`;
    const sibling = `@${JSON.parse(run("new", "--json", "-s", "omp-control")).terminal_id}`;
    const show = (pane = target) => JSON.parse(run("agent", "show", "--json", pane)).agents[0];
    const before = show(sibling);
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
    await runner.emit({ type: "agent_start" });
    await runner.emit({ type: "tool_execution_start", toolCallId: "smoke", toolName: "fixture", args: {} });
    await runner.emit({ type: "tool_execution_end", toolCallId: "smoke", toolName: "fixture", result: { content: [], details: {} }, isError: false });
    await runner.emit({ type: "agent_end", messages: [], willContinue: true });
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
    assert.ok(!show().sources.some((source: { kind: string }) => source.kind === "agent_record"));
    assert.deepEqual(show(sibling).agent, before.agent);
    assert.equal(show(sibling).agent_session, before.agent_session);
    console.log("Private server + packed locked OMP SDK: identity, scripted activity, automatic new/switch/reload/branch, shutdown and untouched sibling passed; no model calls.");
  } finally {
    await session?.dispose();
    server.kill();
    await server.exited;
  }
}
