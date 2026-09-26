import { loadDesktopHost } from "../../native/loader.mjs";
import { registerCustomElementType } from "../../toolchain/gpuix/packages/native/dist/host.js";

// Schema registration before native installation proves it cannot seal startup.
registerCustomElementType("phux-host-probe");
globalThis.solidElementsHost = loadDesktopHost(process.env.PHUX_DESKTOP_ADDON);
