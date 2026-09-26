import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { buildDesktopBundle, prepareDesktopFramework } from "./desktop-bundle";

const root = resolve(import.meta.dir, "..");
const output = resolve(root, "dist/desktop");
const addon =
  process.env.PHUX_DESKTOP_ADDON ??
  resolve(root, ".cache/host/phux-desktop-native.darwin-arm64.node");
const socketPath = process.env.PHUX_SOCKET || runningServerSocket();

// Without PHUX_SOCKET, attach to the server the installed `phux` reports, so
// socket resolution (runtime dir, profile) stays owned by phux itself.
function runningServerSocket(): string {
  const status = spawnSync("phux", ["status", "--json"], { encoding: "utf8" });
  if (status.error) {
    throw new Error(`Set PHUX_SOCKET, or put phux on PATH: ${status.error.message}`);
  }
  const parsed: unknown = status.stdout ? JSON.parse(status.stdout) : undefined;
  const socket =
    parsed && typeof parsed === "object" && "socket" in parsed ? parsed.socket : undefined;
  if (status.status !== 0 || typeof socket !== "string") {
    throw new Error("No running phux server; start one with `phux` or set PHUX_SOCKET");
  }
  return socket;
}

prepareDesktopFramework();
await mkdir(output, { recursive: true });
const built = await buildDesktopBundle(resolve(root, "scripts/desktop-main.ts"), output);
if (!built.success) throw new AggregateError(built.logs, "Desktop bundle failed");

const child = spawn(process.execPath, [resolve(output, "desktop-main.js")], {
  cwd: root,
  env: { ...process.env, PHUX_DESKTOP_ADDON: addon, PHUX_SOCKET: socketPath },
  stdio: "inherit",
});
child.once("exit", (code) => process.exit(code ?? 1));
