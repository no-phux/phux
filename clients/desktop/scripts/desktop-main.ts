import { startDesktop } from "./start-desktop";

const addon = process.env.PHUX_DESKTOP_ADDON;
const socketPath = process.env.PHUX_SOCKET;
if (!addon || !socketPath) {
  throw new Error("Set PHUX_DESKTOP_ADDON and PHUX_SOCKET before launching the desktop");
}

await startDesktop({ addon, socketPath, sessionName: process.env.PHUX_SESSION ?? "desktop" });
