import { realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { isAbsolute } from "node:path";

const require = createRequire(import.meta.url);
let loadedPath;
let host;

/** Load once, before importing GPUIX. Both JS surfaces use this canonical file. */
export function loadDesktopHost(addonPath) {
  if (process.env.NAPI_RS_FORCE_WASI || process.env.NAPI_RS_WASI_FLAVOR) {
    throw new Error("Native desktop startup requires NAPI_RS_FORCE_WASI and NAPI_RS_WASI_FLAVOR unset");
  }
  if (!isAbsolute(addonPath)) {
    throw new Error("Desktop addon path must be absolute");
  }
  const canonicalPath = realpathSync(addonPath);
  if (loadedPath && canonicalPath !== loadedPath) {
    throw new Error("A different desktop addon is already loaded");
  }
  if (host) return host;
  process.env.NAPI_RS_NATIVE_LIBRARY_PATH = canonicalPath;
  loadedPath = canonicalPath;
  const binding = require(canonicalPath);
  binding.initializeDesktopHost();
  host = binding;
  return host;
}
