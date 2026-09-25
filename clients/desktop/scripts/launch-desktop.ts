import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import { buildDesktopBundle, prepareDesktopFramework } from "./desktop-bundle";

const root = resolve(import.meta.dir, "..");
const output = resolve(root, "dist/desktop");
const addon = process.env.PHUX_DESKTOP_ADDON ?? resolve(root, ".cache/host/phux-desktop-native.darwin-arm64.node");
const socketPath = process.env.PHUX_SOCKET;
if (!socketPath) throw new Error("PHUX_SOCKET is required");

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
