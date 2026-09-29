/**
 * Load the single native host, register the custom elements, and mount the
 * shell. Shared by the dev entry (`desktop-main.ts`) and the packaged app
 * (`app-main.ts`); only how they find the addon and socket differs.
 */
import { homedir } from "node:os";
import { join } from "node:path";
import { loadDesktopHost } from "../native/loader.mjs";
import { readGhosttyConfig } from "./ghostty-config";
import { fileLayoutStore } from "./layout-store";
import { ensureServer, ensureSession, findPhux, serverSocket } from "./server";

export interface DesktopStart {
  addon: string;
  socketPath: string;
  sessionName: string;
  /** Surfaced in the shell when the server could not be ensured. */
  startupError?: string;
}

/**
 * Reconnect's recovery: start the server the way launch did (PHUX_SOCKET or
 * the profile's own socket, never a guessed path) and recreate the home
 * session. A server that now listens elsewhere than this window's socket is
 * reported rather than dialled. Returns the failure for the shell to show.
 */
function restartServer(start: DesktopStart): string | undefined {
  const phux = findPhux(process.env.PHUX_BIN);
  if (!phux) return "Install the phux CLI (~/.local/bin/phux) so the desktop can start a server.";
  try {
    const explicit = process.env.PHUX_SOCKET || undefined;
    ensureServer(phux, explicit);
    const socket = explicit ?? serverSocket(phux);
    if (socket !== start.socketPath)
      return `The server listens on ${socket}, not ${start.socketPath}. Relaunch Phux to use it.`;
    ensureSession(phux, socket, start.sessionName);
    return undefined;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
}

export async function startDesktop(start: DesktopStart): Promise<void> {
  const host = loadDesktopHost(start.addon);
  const native = await import("@gpuix/native/host");
  native.registerCustomElementType("phux-terminal");
  native.registerCustomElementType("phux-drag-region");
  const app = await import("../src/app");
  const state = process.env.XDG_STATE_HOME ?? join(homedir(), ".local/state");
  app.mount(host, {
    socketPath: start.socketPath,
    sessionName: start.sessionName,
    layouts: fileLayoutStore(join(state, "phux-desktop/layout.json")),
    startupError: start.startupError,
    readGhostty: readGhosttyConfig,
    quickLayouts: fileLayoutStore(join(state, "phux-desktop/quick.json")),
    ensureServer: () => restartServer(start),
  });
  scheduleCapture(process.env.PHUX_DESKTOP_CAPTURE);
}

/**
 * `PHUX_DESKTOP_CAPTURE=<ms>:<absolute png path>` renders the main window to a
 * PNG after a delay, through GPUI rather than the window server, so packaged
 * builds can be checked without focusing or screenshotting the desktop.
 */
function scheduleCapture(spec: string | undefined): void {
  const match = spec ? /^(\d+):(\/.+\.png)$/.exec(spec) : null;
  if (!match?.[1] || !match[2]) return;
  const path = match[2];
  setTimeout(() => {
    const slot: unknown = Reflect.get(globalThis, Symbol.for("@gpuix/solid/render-host"));
    const renderer: unknown =
      slot && typeof slot === "object" ? Reflect.get(slot, "renderer") : undefined;
    const capture: unknown = renderer ? Reflect.get(renderer, "captureScreenshot") : undefined;
    if (typeof capture === "function") Reflect.apply(capture, renderer, [path]);
  }, Number(match[1]));
}
