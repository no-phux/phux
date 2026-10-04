import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { PhuxCli } from "../dist/src/adapter.js";
import { registerPhuxLifecycle } from "../dist/src/lifecycle.js";
import { PhuxTargetStore } from "../dist/src/target-store.js";
import { PhuxContextAwareness } from "../dist/src/awareness.js";

if (process.env.PHUX_PI_REAL_SMOKE !== "1") {
  process.stdout.write("real smoke skipped; set PHUX_PI_REAL_SMOKE=1 to opt in\n");
  process.exit(0);
}

const phux = process.env.PHUX ?? "phux";
const temp = await mkdtemp(join(tmpdir(), "phux-pi-real-smoke-"));
const socket = join(temp, "runtime", "phux.sock");
const session = "pi-package-smoke";
const env = { ...process.env };
for (const name of [
  "PHUX_WS_ADDR", "PHUX_QUIC_ADDR", "PHUX_WT_ADDR",
  "PHUX_WS_TLS_CERT", "PHUX_WS_TLS_KEY", "PHUX_WS_TOKENS",
]) {
  delete env[name];
}
Object.assign(env, {
  PHUX_SOCKET: socket,
  XDG_CACHE_HOME: join(temp, "cache"),
  XDG_CONFIG_HOME: join(temp, "config"),
  XDG_DATA_HOME: join(temp, "data"),
  XDG_RUNTIME_DIR: join(temp, "runtime"),
  XDG_STATE_HOME: join(temp, "state"),
});
let server;
let serverStderr = "";
let cleanupPromise;
let terminating = false;
const signalHandlers = new Map();
for (const signal of ["SIGINT", "SIGTERM"]) {
  const handler = () => { void handleTerminationSignal(signal); };
  signalHandlers.set(signal, handler);
  process.once(signal, handler);
}

try {
  const version = run(phux, ["--version"], env).stdout.trim();
  const match = /^phux\s+v?(\d+)\.(\d+)\.(\d+)/.exec(version);
  assert.ok(match, `unexpected phux version output: ${JSON.stringify(version)}`);
  const [major, minor, patch] = match.slice(1, 4).map(Number);
  const compatible = major > 0 ||
    (major === 0 && (minor > 1 || (minor === 1 && patch >= 0 && !version.includes("-"))));
  assert.ok(compatible, `real smoke requires phux >= 0.1.0, got ${version}`);

  server = spawn(phux, ["server", "--socket", socket, "--session", "pi-smoke-bootstrap"], {
    env,
    stdio: ["ignore", "ignore", "pipe"],
  });
  server.stderr.setEncoding("utf8");
  server.stderr.on("data", (chunk) => { serverStderr = `${serverStderr}${chunk}`.slice(-16_384); });

  await waitForServer(phux, socket, env, server);
  const created = JSON.parse(run(phux, [
    "new", "--json", "-s", session, "--socket", socket,
  ], env).stdout);
  assert.equal(created.session, session);
  const target = `@${String(created.terminal_id)}`;

  const marker = "PHUX_PI_SMOKE_OK";
  const commandResult = run(phux, [
    "run", "--json", "--timeout", "10", "--socket", socket,
    target, `printf '${marker}\\n'`,
  ], env, true);
  const command = JSON.parse(commandResult.stdout);
  assert.equal(command.exit_code, 0);
  assert.match(command.output, new RegExp(marker));

  const snapshot = JSON.parse(run(phux, [
    "snapshot", "--json", "--scrollback", "20", "--socket", socket, target,
  ], env).stdout);
  assert.equal(snapshot.pane, created.terminal_id);
  assert.match([...snapshot.scrollback, ...snapshot.lines].join("\n"), new RegExp(marker));

  await verifyHostingLifecycle(target);

  const attachArgv = ["phux", "attach", "--socket", socket, session];
  process.stdout.write(`created ${session} ${target}; run exit=${String(command.exit_code)}; snapshot=${String(snapshot.cols)}x${String(snapshot.rows)}\n`);
  process.stdout.write(`human attach argv: ${JSON.stringify(attachArgv)}\n`);
} finally {
  await cleanup();
  if (!terminating) removeSignalHandlers();
}

async function verifyHostingLifecycle(target) {
  const cli = new PhuxCli({ executable: phux, socket, env });
  const store = new PhuxTargetStore({ appendEntry() {} }, cli);
  await store.refresh();
  const sibling = store.panes.find((pane) => pane.terminal !== target);
  assert.ok(sibling, "bootstrap pane supplies a distinct control target");
  store.select(sibling);
  const handlers = new Map();
  const pending = new Set();
  const errors = [];
  const { lifecycle } = registerPhuxLifecycle({ on(name, handler) { handlers.set(name, handler); } }, store, {
    cli, hostTerminal: target, onError(error) { errors.push(error); },
    timers: {
      setTimeout(callback) { pending.add(callback); return callback; },
      clearTimeout(callback) { pending.delete(callback); },
    },
  });
  const ctx = { sessionManager: { getSessionId: () => "pi-smoke-host-session" } };
  await handlers.get("session_start")({ reason: "startup" }, ctx);
  for (const callback of pending) { pending.delete(callback); callback(); }
  await lifecycle.settled();
  assert.deepEqual(errors, [], "hosting declaration and AgentSession must succeed");
  const host = (await cli.agentShow({ target })).agents[0];
  assert.equal(host.agent.kind, "pi");
  assert.equal(host.agent_session.provider, "pi");
  assert.equal(host.agent_session.native_id, "pi-smoke-host-session");
  const control = (await cli.agentShow({ target: sibling.terminal })).agents[0];
  assert.deepEqual(control.agent, sibling.agent, "control target must not be relabeled");
  const awareness = new PhuxContextAwareness(cli);
  const checkpoint = await awareness.next("smoke", { self: target, selected: sibling.terminal });
  assert.match(checkpoint.text, /"availability":"available"/);
  assert.match(checkpoint.text, /"agent_session":\{/);
  await handlers.get("session_shutdown")({ reason: "quit" }, ctx);
  assert.deepEqual(errors, [], "host cleanup must succeed");
  assert.equal((await cli.agentShow({ target })).agents[0].agent_session, null);
  process.stdout.write(`verified Pi host ${target}, independent control ${sibling.terminal}, AgentSession and context\n`);
}

async function cleanup() {
  cleanupPromise ??= cleanupOnce();
  return cleanupPromise;
}

async function cleanupOnce() {
  const child = server;
  try {
    if (child !== undefined && !hasExited(child)) {
      spawnSync(phux, ["kill", "--socket", socket, session], {
        env,
        encoding: "utf8",
        timeout: 5_000,
      });
      child.kill("SIGTERM");
      await Promise.race([onceExit(child), delay(3_000)]);
      if (!hasExited(child)) {
        child.kill("SIGKILL");
        await Promise.race([onceExit(child), delay(3_000)]);
      }
      if (!hasExited(child)) throw new Error("private phux server did not terminate");
    }
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
}

async function handleTerminationSignal(signal) {
  if (terminating) return;
  terminating = true;
  try {
    await cleanup();
  } catch (error) {
    process.stderr.write(`real smoke cleanup failed after ${signal}: ${error instanceof Error ? error.message : String(error)}\n`);
    removeSignalHandlers();
    process.exit(1);
  }
  removeSignalHandlers();
  process.kill(process.pid, signal);
}

function removeSignalHandlers() {
  for (const [signal, handler] of signalHandlers) {
    process.removeListener(signal, handler);
  }
  signalHandlers.clear();
}

async function waitForServer(executable, socketPath, childEnv, child) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (hasExited(child)) {
      throw new Error(`phux server exited early (code=${String(child.exitCode)}, signal=${String(child.signalCode)}): ${serverStderr}`);
    }
    const result = spawnSync(executable, ["ls", "--json", "--socket", socketPath], {
      env: childEnv,
      encoding: "utf8",
      timeout: 2_000,
    });
    if (result.status === 0) return;
    await delay(25);
  }
  throw new Error(`phux server did not become ready: ${serverStderr}`);
}

function run(command, args, childEnv, allowChildExit = false) {
  const result = spawnSync(command, args, { env: childEnv, encoding: "utf8", timeout: 30_000 });
  if (result.error !== undefined) throw result.error;
  if ((!allowChildExit && result.status !== 0) || (allowChildExit && result.stdout.trim().length === 0)) {
    throw new Error(`${command} ${args.join(" ")} failed (${String(result.status)}): ${result.stderr}`);
  }
  return result;
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function hasExited(child) {
  return child.exitCode !== null || child.signalCode !== null;
}

function onceExit(child) {
  if (hasExited(child)) return Promise.resolve();
  return new Promise((resolve) => child.once("exit", resolve));
}
