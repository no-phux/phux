import assert from "node:assert/strict";
import { realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { loadDesktopHost } from "../../native/loader.mjs";

// Run in a fresh process: a native view created before explicit bootstrap
// permanently closes installation, even if no extension has yet been installed.
assert.ok(process.env.PHUX_DESKTOP_ADDON);
const addonPath = realpathSync(process.env.PHUX_DESKTOP_ADDON);
// SAFETY: this test requires the exact source-built addon whose declarations are freshness-checked.
const host = createRequire(import.meta.url)(
  addonPath,
) as typeof import("../../native/generated/index");
new host.TestGpuixRenderer(100, 100);
assert.throws(() => host.initializeDesktopHost(), /before creating a registry/);
assert.throws(() => loadDesktopHost(addonPath), /before creating a registry/);
assert.throws(() => loadDesktopHost(addonPath), /before creating a registry/);
console.log("PASS: view-before-bootstrap rejects late extension installation");
