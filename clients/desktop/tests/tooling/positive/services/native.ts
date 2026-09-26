import { resolve } from "node:path";
import { createRequire } from "node:module";

export const nativePath = resolve("dist", "phux-desktop.node");
export const loadNative = createRequire(import.meta.url);
