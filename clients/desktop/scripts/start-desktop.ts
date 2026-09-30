/**
 * Load the single native host, register the custom elements, and mount the
 * shell. Shared by the dev entry (`desktop-main.ts`) and the packaged app
 * (`app-main.ts`); only how they find the addon and socket differs.
 */
import { appendFileSync, mkdtempSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, join } from "node:path";
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

/**
 * Ghostty's `write_*_file` target: a fresh private directory under the
 * temporary directory per write, so no path is ever reused or shared.
 */
function writeTempFile(name: string, text: string): string {
  const path = join(mkdtempSync(join(tmpdir(), "phux-")), basename(name));
  writeFileSync(path, text, { mode: 0o600 });
  return path;
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
    writeTempFile,
  });
  scheduleCapture(process.env.PHUX_DESKTOP_CAPTURE);
  const { drainStats } = await import("../src/bridge/desktop");
  schedulePerfLog(process.env.PHUX_DESKTOP_PERF, host.desktopPerfJson, drainStats);
}

/** The main window's GPUIX renderer, once `app.mount` has created it. */
function mainRenderer(): object | undefined {
  const slot: unknown = Reflect.get(globalThis, Symbol.for("@gpuix/solid/render-host"));
  const renderer: unknown =
    slot && typeof slot === "object" ? Reflect.get(slot, "renderer") : undefined;
  return renderer && typeof renderer === "object" ? renderer : undefined;
}

function callRenderer(name: string): unknown {
  const renderer = mainRenderer();
  const method: unknown = renderer ? Reflect.get(renderer, name) : undefined;
  return typeof method === "function" ? Reflect.apply(method, renderer, []) : undefined;
}

/**
 * `PHUX_DESKTOP_PERF=<absolute path>` appends one JSON line per second: the
 * main window's draw count and recent draw times (GPUI's frame overlay
 * numbers, over its last 1000 draws; resetting them would itself redraw),
 * the wake drains in that second, and the host's cumulative kernel, runtime
 * and painter metrics (`desktopPerfJson`). Diagnostics only; unset, nothing
 * runs.
 */
function schedulePerfLog(
  path: string | undefined,
  hostPerf: () => string,
  drains: { wakes: number; events: number; ms: number; maxMs: number },
): void {
  if (!path?.startsWith("/")) return;
  setInterval(() => {
    const drained = { ...drains };
    Object.assign(drains, { wakes: 0, events: 0, ms: 0, maxMs: 0 });
    // Diagnostics never take the app down, not even once its main window
    // has closed and the renderer refuses every call.
    try {
      const host: unknown = JSON.parse(hostPerf());
      const frames = callRenderer("getDebugFrameOverlayStats");
      appendFileSync(
        path,
        `${JSON.stringify({ at: Date.now(), frames, drains: drained, host })}\n`,
      );
    } catch {
      // Skip this second.
    }
  }, 1000);
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
    const renderer = mainRenderer();
    const capture: unknown = renderer ? Reflect.get(renderer, "captureScreenshot") : undefined;
    if (typeof capture === "function") Reflect.apply(capture, renderer, [path]);
  }, Number(match[1]));
}
