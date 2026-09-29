import { existsSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import { buildDesktopBundle, prepareDesktopFramework } from "./desktop-bundle";
import { ensureServer, ensureSession, serverSocket } from "./server";

const root = resolve(import.meta.dir, "..");
const repo = resolve(root, "../..");
const output = resolve(root, "dist/desktop");
const addon =
  process.env.PHUX_DESKTOP_ADDON ??
  resolve(root, ".cache/host/phux-desktop-native.darwin-arm64.node");
const phux = phuxBinary();
// `phux server --ensure` owns the socket. Status only reads it back.
ensureServer(phux, process.env.PHUX_SOCKET);
const socketPath = process.env.PHUX_SOCKET || serverSocket(phux);
const sessionName = process.env.PHUX_SESSION ?? "desktop";
ensureSession(phux, socketPath, sessionName);

function phuxBinary(): string {
  if (process.env.PHUX_BIN) return process.env.PHUX_BIN;
  const built = resolve(repo, "target/debug/phux");
  if (existsSync(built)) return built;
  return "phux";
}

prepareDesktopFramework();
await mkdir(output, { recursive: true });
const built = await buildDesktopBundle(resolve(root, "scripts/desktop-main.ts"), output);
if (!built.success) throw new AggregateError(built.logs, "Desktop bundle failed");

const child = spawn(process.execPath, [resolve(output, "desktop-main.js")], {
  cwd: root,
  env: {
    ...process.env,
    PHUX_DESKTOP_ADDON: addon,
    // Reconnect restarts a stopped server with the same CLI that started it.
    PHUX_BIN: phux,
    PHUX_SOCKET: socketPath,
    PHUX_SESSION: sessionName,
  },
  stdio: "inherit",
});
child.once("exit", (code) => process.exit(code ?? 1));
