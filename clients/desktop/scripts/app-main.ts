/**
 * Entry of the packaged `Phux.app`. The compiled executable sits in
 * Contents/MacOS; the native addon ships beside it in Contents/Resources.
 * It uses the installed `phux` CLI to ensure the user's server (honouring
 * PHUX_PROFILE and PHUX_SOCKET), then attaches the `default` session unless
 * PHUX_SESSION names another. A startup failure still opens the window, with
 * the reason and a Reconnect button, rather than exiting silently.
 */
import { appendFileSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { ensureServer, ensureSession, findPhux, pickSession, serverSocket } from "./server";
import { startDesktop } from "./start-desktop";

const logFile = join(homedir(), "Library/Logs/phux-desktop.log");

function log(message: string): void {
  try {
    mkdirSync(dirname(logFile), { recursive: true });
    appendFileSync(logFile, `${new Date().toISOString()} ${message}\n`);
  } catch {
    // Logging must never take the app down.
  }
}

process.on("uncaughtException", (error) => {
  log(`uncaught: ${error.stack ?? String(error)}`);
  process.exit(1);
});

const resources = resolve(dirname(process.execPath), "../Resources");
const addon =
  process.env.PHUX_DESKTOP_ADDON ?? join(resources, "phux-desktop-native.darwin-arm64.node");
const fallbackSocket =
  process.env.PHUX_SOCKET ?? `/tmp/phux-${process.env.USER ?? "user"}/phux.sock`;

function discover(): { socketPath: string; sessionName: string; startupError?: string } {
  const phux = findPhux(process.env.PHUX_BIN);
  if (!phux) {
    return {
      socketPath: fallbackSocket,
      sessionName: process.env.PHUX_SESSION ?? "default",
      startupError:
        "Install the phux CLI from https://phux.sh/install so the desktop can start a server.",
    };
  }
  try {
    ensureServer(phux, process.env.PHUX_SOCKET);
    const socketPath = process.env.PHUX_SOCKET || serverSocket(phux);
    const sessionName = pickSession(phux, socketPath, process.env.PHUX_SESSION);
    ensureSession(phux, socketPath, sessionName);
    log(`attach ${sessionName} on ${socketPath} via ${phux}`);
    return { socketPath, sessionName };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    log(`startup: ${message}`);
    return {
      socketPath: fallbackSocket,
      sessionName: process.env.PHUX_SESSION ?? "default",
      startupError: message,
    };
  }
}

await startDesktop({ addon, ...discover() });
