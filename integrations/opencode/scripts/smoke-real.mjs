import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

if (process.env.PHUX_OPENCODE_REAL_SMOKE !== "1") {
  process.stdout.write("real smoke skipped; set PHUX_OPENCODE_REAL_SMOKE=1 to opt in\n");
  process.exit(0);
}

const packageRoot = new URL("../", import.meta.url);
const phux = process.env.PHUX ?? "phux";
const temporaryRoot = await mkdtemp(join(tmpdir(), "phux-opencode-real-smoke-"));
const socket = join(temporaryRoot, "runtime", "phux.sock");
const session = "opencode-package-smoke";
const remoteServerVariables = [
  "PHUX_WS_ADDR",
  "PHUX_QUIC_ADDR",
  "PHUX_WT_ADDR",
  "PHUX_WS_TLS_CERT",
  "PHUX_WS_TLS_KEY",
  "PHUX_WS_TOKENS",
];
const localEnvironment = { ...process.env };
for (const variable of remoteServerVariables) delete localEnvironment[variable];
const environment = {
  ...localEnvironment,
  PHUX_SOCKET: socket,
  XDG_CACHE_HOME: join(temporaryRoot, "cache"),
  XDG_CONFIG_HOME: join(temporaryRoot, "config"),
  XDG_DATA_HOME: join(temporaryRoot, "data"),
  XDG_RUNTIME_DIR: join(temporaryRoot, "runtime"),
  XDG_STATE_HOME: join(temporaryRoot, "state"),
};
for (const variable of remoteServerVariables) {
  assert.equal(Object.hasOwn(environment, variable), false, `${variable} must not reach the private server`);
}
const abort = new AbortController();
const signalHandlers = new Map();
let server;
let serverStderr = "";
let hooks;
let pluginDisposed = false;
let cleanupPromise;
let terminating = false;

for (const signal of ["SIGINT", "SIGTERM"]) {
  const handler = () => { void handleTerminationSignal(signal); };
  signalHandlers.set(signal, handler);
  process.once(signal, handler);
}

try {
  assertCompatibleVersion(run(phux, ["--version"], environment).stdout.trim());

  run("npm", ["run", "build"], process.env, packageRoot);
  const packed = JSON.parse(run("npm", [
    "pack", "--json", "--ignore-scripts", "--pack-destination", temporaryRoot,
  ], process.env, packageRoot).stdout);
  assert.equal(packed.length, 1, "npm pack must produce one artifact");
  const tarball = join(temporaryRoot, packed[0].filename);
  const consumerRoot = join(temporaryRoot, "consumer");
  run("npm", [
    "install", "--ignore-scripts", "--omit=dev", "--prefix", consumerRoot, tarball,
  ], process.env);

  const installedEntry = join(
    consumerRoot, "node_modules", "@phux", "opencode", "dist", "index.js",
  );
  const pluginModule = await import(pathToFileURL(installedEntry).href);
  assert.equal(typeof pluginModule.default, "function", "packed public plugin entry must load");

  server = spawn(phux, [
    "server", "--socket", socket, "--session", "opencode-smoke-bootstrap",
  ], { env: environment, stdio: ["ignore", "ignore", "pipe"] });
  server.stderr.setEncoding("utf8");
  server.stderr.on("data", (chunk) => {
    serverStderr = `${serverStderr}${chunk}`.slice(-16_384);
  });
  await waitForServer();

  hooks = await pluginModule.default({}, {
    executable: phux,
    socket,
    env: environment,
  });
  assert.deepEqual(Object.keys(hooks.tool).sort(), [
    "phux_create", "phux_list", "phux_run", "phux_send_keys", "phux_snapshot", "phux_wait",
  ]);

  const toolContext = {
    sessionID: "opencode-real-smoke",
    messageID: "no-llm",
    agent: "smoke",
    directory: temporaryRoot,
    worktree: temporaryRoot,
    abort: abort.signal,
    metadata() {},
    async ask() {},
  };
  const created = await hooks.tool.phux_create.execute({ name: session }, toolContext);
  assert.equal(created.metadata.operation, "create");
  assert.match(created.output, new RegExp(`Created ${session} at @\\d+`));
  const target = created.metadata.target;
  assert.match(target, /^@\d+$/);

  const message = {
    id: "phux-context-smoke-message",
    sessionID: toolContext.sessionID,
    role: "user",
    time: { created: Date.now() },
    agent: "smoke",
    model: { providerID: "offline", modelID: "offline" },
  };
  const contextOutput = {
    message,
    parts: [{
      id: "phux-context-smoke-user-text",
      sessionID: message.sessionID,
      messageID: message.id,
      type: "text",
      text: "Inspect the fleet without calling a model.",
    }],
  };
  await hooks["chat.message"]({ sessionID: message.sessionID, messageID: message.id }, contextOutput);
  assert.equal(contextOutput.parts.length, 2, "real agent inventory must append one synthetic context part");
  assert.equal(contextOutput.parts[1].synthetic, true);
  assert.match(contextOutput.parts[1].text, /kind="checkpoint" seq="1"/);
  assert.match(contextOutput.parts[1].text, new RegExp(`"selected":"${target.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"`));
  assert.doesNotMatch(contextOutput.parts[1].text, /"title"|"sources"|"explanation"/);

  const marker = "PHUX_OPENCODE_SMOKE_OK";
  const command = await hooks.tool.phux_run.execute({
    command: `printf '${marker}\\n'`,
    timeout_seconds: 10,
  }, toolContext);
  assert.equal(command.metadata.operation, "run");
  assert.equal(command.metadata.target, target, "create must select the run target");
  assert.equal(command.metadata.exitCode, 0);
  assert.match(command.output, new RegExp(marker));

  const snapshot = await hooks.tool.phux_snapshot.execute({ scrollback: 20 }, toolContext);
  assert.equal(snapshot.metadata.operation, "snapshot");
  assert.equal(snapshot.metadata.target, target, "snapshot must reuse the selected target");
  assert.match(snapshot.output, new RegExp(marker));

  // This is guidance for a separate human terminal, not a command the plugin
  // executes. Keeping it as argv avoids suggesting eval of a shell string.
  const attachArgv = [phux, "attach", "--socket", socket, session];
  assert.deepEqual(attachArgv.slice(1), ["attach", "--socket", socket, session]);
  assert.equal(attachArgv.some((arg) => /(?:--quic|--ws|--token|--cert-fingerprint)/.test(arg)), false);

  process.stdout.write(
    `packed OpenCode plugin created ${session} ${target}; run exit=0; snapshot=${String(snapshot.metadata.cols ?? "bounded")}x${String(snapshot.metadata.rows ?? "bounded")}\n`,
  );
  process.stdout.write(`human attach argv (run in a separate terminal): ${JSON.stringify(attachArgv)}\n`);
} finally {
  await cleanup();
  if (!terminating) removeSignalHandlers();
}

async function cleanup() {
  cleanupPromise ??= cleanupOnce();
  return cleanupPromise;
}

async function cleanupOnce() {
  abort.abort();
  try {
    if (hooks !== undefined && !pluginDisposed) {
      pluginDisposed = true;
      await hooks.dispose?.();
    }
  } finally {
    const child = server;
    try {
      if (child !== undefined && !hasExited(child)) {
        spawnSync(phux, ["kill", "--socket", socket, session], {
          env: environment,
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
      await rm(temporaryRoot, { recursive: true, force: true });
    }
  }
}

async function handleTerminationSignal(signal) {
  if (terminating) return;
  terminating = true;
  try {
    await cleanup();
  } catch (error) {
    process.stderr.write(
      `real smoke cleanup failed after ${signal}: ${error instanceof Error ? error.message : String(error)}\n`,
    );
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

async function waitForServer() {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (hasExited(server)) {
      throw new Error(
        `phux server exited early (code=${String(server.exitCode)}, signal=${String(server.signalCode)}): ${serverStderr}`,
      );
    }
    const result = spawnSync(phux, ["ls", "--json", "--socket", socket], {
      env: environment,
      encoding: "utf8",
      timeout: 2_000,
    });
    if (result.status === 0) return;
    await delay(25);
  }
  throw new Error(`private phux server did not become ready: ${serverStderr}`);
}

function run(command, args, childEnvironment, cwd) {
  const result = spawnSync(command, args, {
    env: childEnvironment,
    ...(cwd === undefined ? {} : { cwd }),
    encoding: "utf8",
    timeout: 120_000,
  });
  if (result.error !== undefined) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed (${String(result.status)}): ${result.stderr}`);
  }
  return result;
}

function assertCompatibleVersion(version) {
  const match = /^phux\s+v?(\d+)\.(\d+)\.(\d+)/.exec(version);
  assert.ok(match, `unexpected phux version output: ${JSON.stringify(version)}`);
  const [major, minor, patch] = match.slice(1, 4).map(Number);
  const compatible = major > 0 ||
    (major === 0 && (minor > 1 || (minor === 1 && patch >= 0 && !version.includes("-"))));
  assert.ok(compatible, `real smoke requires phux >= 0.1.0, got ${version}`);
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function hasExited(child) {
  return child.exitCode !== null || child.signalCode !== null;
}

function onceExit(child) {
  if (hasExited(child)) return Promise.resolve();
  return new Promise((resolve) => child.once("exit", resolve));
}
