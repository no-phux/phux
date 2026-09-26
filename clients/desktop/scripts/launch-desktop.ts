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
const sessionName = process.env.PHUX_SESSION ?? "desktop";
ensureSession(socketPath, sessionName);

// Without PHUX_SOCKET, attach to the server the installed `phux` reports, so
// socket resolution (runtime dir, profile) stays owned by phux itself.
function runningServerSocket(): string {
  const status = phuxJson(["status", "--json"]);
  const socket = status && "socket" in status ? status.socket : undefined;
  if (typeof socket !== "string") {
    throw new Error("No running phux server; start one with `phux` or set PHUX_SOCKET");
  }
  return socket;
}

// The desktop host only attaches to an existing session, so create the one it
// is about to open when the server does not have it yet.
function ensureSession(socket: string, name: string): void {
  const status = phuxJson(["--socket", socket, "status", "--json"]);
  const sessions = status && "sessions" in status ? status.sessions : undefined;
  if (!Array.isArray(sessions)) {
    throw new Error(`No running phux server at ${socket}`);
  }
  const exists = sessions.some(
    (session: unknown) =>
      session !== null && typeof session === "object" && "name" in session && session.name === name,
  );
  if (!exists && !phuxJson(["--socket", socket, "new", "--json", "-s", name])) {
    throw new Error(`Could not create phux session "${name}"`);
  }
}

function phuxJson(args: string[]): object | undefined {
  const run = spawnSync("phux", args, { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
  if (run.error) {
    throw new Error(`Set PHUX_SOCKET, or put phux on PATH: ${run.error.message}`);
  }
  if (run.status !== 0 || !run.stdout) return undefined;
  const parsed: unknown = JSON.parse(run.stdout);
  return parsed !== null && typeof parsed === "object" ? parsed : undefined;
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
    PHUX_SOCKET: socketPath,
    PHUX_SESSION: sessionName,
  },
  stdio: "inherit",
});
child.once("exit", (code) => process.exit(code ?? 1));
