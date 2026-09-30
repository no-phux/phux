import { spawn, spawnSync } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { mkdir, mkdtemp, realpath, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, relative, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { launchDesktop, prepareDesktop } from "./launch-desktop";
import { terminateChild } from "./terminate-child";

const desktop = resolve(import.meta.dir, "..");
const repo = resolve(desktop, "../..");

interface Artifacts {
  binary: string;
  addon: string;
}

function within(parent: string, child: string): boolean {
  const path = relative(parent, child);
  return path === "" || (path !== ".." && !path.startsWith("../") && !isAbsolute(path));
}

export async function checkoutArtifacts(
  binary = resolve(repo, process.env.CARGO_TARGET_DIR ?? "target", "debug/phux"),
  addon = resolve(desktop, ".cache/host/phux-desktop-native.darwin-arm64.node"),
): Promise<Artifacts> {
  const [checkout, cli, native] = await Promise.all([
    realpath(repo),
    realpath(binary),
    realpath(addon),
  ]);
  if (!within(checkout, cli) || !within(checkout, native)) {
    throw new Error(
      "Demo requires checkout-built CLI and addon (no installed or external artifacts)",
    );
  }
  if (!(await stat(cli)).isFile() || !(await stat(native)).isFile()) {
    throw new Error("Demo CLI and addon must be regular files");
  }
  return { binary: cli, addon: native };
}

async function demoEnvironment(sandbox: string, artifacts: Artifacts): Promise<NodeJS.ProcessEnv> {
  const env: NodeJS.ProcessEnv = Object.fromEntries(
    Object.entries(process.env).filter(
      ([key]) => !key.startsWith("PHUX_") && !key.startsWith("XDG_"),
    ),
  );
  const dirs = {
    HOME: sandbox,
    TMPDIR: resolve(sandbox, "tmp"),
    XDG_RUNTIME_DIR: sandbox,
    XDG_CONFIG_HOME: resolve(sandbox, "config"),
    XDG_CONFIG_DIRS: resolve(sandbox, "config"),
    XDG_STATE_HOME: resolve(sandbox, "state"),
    XDG_DATA_HOME: resolve(sandbox, "data"),
    XDG_DATA_DIRS: resolve(sandbox, "data"),
    XDG_CACHE_HOME: resolve(sandbox, "cache"),
  };
  await Promise.all(
    Object.values(dirs).map((path) => mkdir(path, { recursive: true, mode: 0o700 })),
  );
  return {
    ...env,
    ...dirs,
    PHUX_PROFILE: "desktop-demo",
    PHUX_SOCKET: resolve(sandbox, "demo.sock"),
    PHUX_SESSION: "desktop-demo",
    PHUX_BIN: artifacts.binary,
    PHUX_DESKTOP_ADDON: artifacts.addon,
    PHUX_DESKTOP_DEMO: "1",
    PHUX_NO_AUTO_LISTEN: "1",
  };
}

async function waitForServer(
  binary: string,
  env: NodeJS.ProcessEnv,
  signal: AbortSignal,
): Promise<void> {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    signal.throwIfAborted();
    const status = spawnSync(binary, ["--socket", env.PHUX_SOCKET ?? "", "status", "--json"], {
      env,
      stdio: "ignore",
      timeout: Math.max(1, Math.min(1_000, deadline - Date.now())),
      killSignal: "SIGKILL",
    });
    if (status.status === 0) return;
    await delay(100, undefined, { signal });
  }
  throw new Error("Private demo server did not become ready within 10 seconds");
}

async function reap(children: ChildProcess[], sandbox: string): Promise<void> {
  // Both children begin shutdown together; an uncooperative app cannot strand the server.
  const results = await Promise.allSettled(children.map((child) => terminateChild(child)));
  const failures: unknown[] = [];
  for (const result of results) {
    if (result.status === "rejected") failures.push(result.reason);
  }
  if (failures.length > 0) {
    throw new AggregateError(
      failures,
      `Demo child did not exit; retained private sandbox: ${sandbox}`,
    );
  }
  await rm(sandbox, { recursive: true, force: true });
}

/** The synchronous launch seam returns the actual app, never a launcher grandchild. */
export async function runDemo(
  artifacts: Artifacts,
  launch: (env: NodeJS.ProcessEnv) => ChildProcess,
): Promise<number> {
  const checked = await checkoutArtifacts(artifacts.binary, artifacts.addon);
  const sandbox = await mkdtemp(resolve(tmpdir(), "phux-demo-"));
  const children: ChildProcess[] = [];
  const controller = new AbortController();
  const { promise: interrupted, resolve: stop } = Promise.withResolvers<number>();
  const onInterrupt = (): void => {
    stop(130);
    controller.abort();
  };
  const onTerminate = (): void => {
    stop(143);
    controller.abort();
  };
  process.on("SIGINT", onInterrupt);
  process.on("SIGTERM", onTerminate);
  try {
    const env = await demoEnvironment(sandbox, checked);
    if (controller.signal.aborted) return await interrupted;
    const server = spawn(
      checked.binary,
      [
        "--socket",
        env.PHUX_SOCKET ?? "",
        "server",
        "--session",
        "desktop-demo",
        "--exit-after-idle",
        "120",
      ],
      { env, cwd: sandbox, stdio: ["ignore", "inherit", "inherit"] },
    );
    children.push(server);
    const serverFailed = new Promise<never>((_, reject) => {
      server.once("error", reject);
      server.once("exit", () => reject(new Error("Private demo server exited")));
    });
    const ready = await Promise.race([
      waitForServer(checked.binary, env, controller.signal),
      interrupted,
      serverFailed,
    ]);
    if (typeof ready === "number") return ready;
    console.log(`Desktop demo: ${env.PHUX_SOCKET} (temporary state: ${sandbox})`);
    const app = launch(env);
    children.push(app);
    const appExited = new Promise<number>((done, reject) => {
      app.once("error", reject);
      app.once("exit", (code) => done(code ?? 1));
    });
    return await Promise.race([appExited, interrupted, serverFailed]);
  } finally {
    controller.abort();
    try {
      await reap(children, sandbox);
    } finally {
      process.off("SIGINT", onInterrupt);
      process.off("SIGTERM", onTerminate);
    }
  }
}

if (import.meta.main) {
  const artifacts = await checkoutArtifacts();
  await prepareDesktop();
  process.exitCode = await runDemo(artifacts, launchDesktop);
}
