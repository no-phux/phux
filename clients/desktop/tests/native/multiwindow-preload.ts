import { loadDesktopHost } from "../../native/loader.mjs";

const addon = process.env.PHUX_DESKTOP_ADDON;
if (!addon) throw new Error("PHUX_DESKTOP_ADDON is required");
globalThis.multiwindowHost = loadDesktopHost(addon);
