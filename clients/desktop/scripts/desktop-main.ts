import { homedir } from "node:os";
import { join } from "node:path";
import { loadDesktopHost } from "../native/loader.mjs";
import { fileLayoutStore } from "./layout-store";

const addon = process.env.PHUX_DESKTOP_ADDON;
const socketPath = process.env.PHUX_SOCKET;
if (!addon || !socketPath) {
  throw new Error("Set PHUX_DESKTOP_ADDON and PHUX_SOCKET before launching the desktop");
}

const host = loadDesktopHost(addon);
const native = await import("@gpuix/native/host");
native.registerCustomElementType("phux-terminal");
const windows = await import("../src/other-window");
const app = await import("../src/app");
globalThis.phuxOpenWindow = (placement) => {
  windows.openOtherWindow(host.GpuixRenderer, placement);
};
const state = process.env.XDG_STATE_HOME ?? join(homedir(), ".local/state");
app.mount(
  host,
  socketPath,
  process.env.PHUX_SESSION ?? "desktop",
  fileLayoutStore(join(state, "phux-desktop/layout.json")),
);
