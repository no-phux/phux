import { existsSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { buildDesktopBundle, prepareDesktopFramework } from "./desktop-bundle";
import { ensureServer, ensureSession, serverSocket, serverStatus } from "./server";

const root = resolve(import.meta.dir, "..");
const repo = resolve(root, "../..");
const output = resolve(root, "dist/desktop");

export async function prepareDesktop(): Promise<void> {
  prepareDesktopFramework();
  await mkdir(output, { recursive: true });
  const built = await buildDesktopBundle(resolve(root, "scripts/desktop-main.ts"), output);
  if (!built.success) throw new AggregateError(built.logs, "Desktop bundle failed");
}

/** Spawn only the app: the demo supervisor owns its server and this direct child. */
export function launchDesktop(env: NodeJS.ProcessEnv): ChildProcess {
  return spawn(process.execPath, [resolve(output, "desktop-main.js")], {
    cwd: root,
    env,
    stdio: "inherit",
  });
}

function phuxBinary(): string {
  if (process.env.PHUX_BIN) return process.env.PHUX_BIN;
  const built = resolve(repo, "target/debug/phux");
  if (existsSync(built)) return built;
  return "phux";
}

if (import.meta.main) {
  const addon =
    process.env.PHUX_DESKTOP_ADDON ??
    resolve(root, ".cache/host/phux-desktop-native.darwin-arm64.node");
  const phux = phuxBinary();
  const demo = process.env.PHUX_DESKTOP_DEMO === "1";
  if (demo) {
    // Never replace the supervisor's server, including after an early exit.
    if (!process.env.PHUX_BIN || !process.env.PHUX_SOCKET || !process.env.PHUX_DESKTOP_ADDON)
      throw new Error("Demo launcher requires an explicit CLI, socket, and addon");
    if (!serverStatus(phux, process.env.PHUX_SOCKET))
      throw new Error("Private demo server is not running");
  } else {
    ensureServer(phux, process.env.PHUX_SOCKET);
  }
  const socketPath = process.env.PHUX_SOCKET || serverSocket(phux);
  const sessionName = process.env.PHUX_SESSION ?? "desktop";
  if (!demo) ensureSession(phux, socketPath, sessionName);
  await prepareDesktop();
  const child = launchDesktop({
    ...process.env,
    PHUX_DESKTOP_ADDON: addon,
    PHUX_BIN: phux,
    PHUX_SOCKET: socketPath,
    PHUX_SESSION: sessionName,
  });
  child.once("error", (error) => {
    console.error(error);
    process.exitCode = 1;
  });
  child.once("exit", (code) => process.exit(code ?? 1));
}
