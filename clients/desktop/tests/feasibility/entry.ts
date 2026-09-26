import { loadDesktopHost } from "../../native/loader.mjs";

const addon = process.env.PHUX_DESKTOP_ADDON;
const socket = process.env.PHUX_FEASIBILITY_SOCKET;
if (!addon || !socket) throw new Error("Fixture requires addon and isolated socket paths");

// The canonical combined addon installs its factory before GPUIX is imported.
const host = loadDesktopHost(addon);
const { mount } = await import("./window");
mount(new host.DesktopClient(), socket);
