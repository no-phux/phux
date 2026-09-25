import assert from "node:assert/strict";
import { mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { loadDesktopHost } from "../../native/loader.mjs";

assert.ok(process.env.PHUX_DESKTOP_ADDON);
const addonPath = realpathSync(process.env.PHUX_DESKTOP_ADDON);
for (const name of ["NAPI_RS_FORCE_WASI", "NAPI_RS_WASI_FLAVOR"]) {
  process.env[name] = "error";
  assert.throws(() => loadDesktopHost(addonPath), /requires NAPI_RS_FORCE_WASI/);
  delete process.env[name];
}
// Only view creation seals installation: the production constructor is inert.
const require = createRequire(import.meta.url);
// SAFETY: the release addon is generated from the checked-in native declaration source.
const rawHost = require(addonPath) as typeof import("../../native/generated/index");
const uninitializedRenderer = new rawHost.GpuixRenderer();
const host = loadDesktopHost(addonPath);
assert.equal(typeof uninitializedRenderer.init, "function");
assert.equal(require(addonPath), host);
assert.equal(loadDesktopHost(addonPath), host);
assert.throws(() => loadDesktopHost("relative.node"), /must be absolute/);
const aliases = mkdtempSync(resolve(import.meta.dirname, "../../.cache/loader-"));
try {
  const alias = resolve(aliases, "same.node");
  symlinkSync(addonPath, alias);
  assert.equal(loadDesktopHost(alias), host);
  const other = resolve(aliases, "different.node");
  writeFileSync(other, "must never reach dlopen");
  assert.throws(() => loadDesktopHost(other), /different desktop addon/);
} finally {
  rmSync(aliases, { recursive: true });
}
assert.equal(host.hasTestGpuixRenderer(), true);
assert.equal(typeof host.GpuixRenderer, "function");
assert.equal(typeof host.TestGpuixRenderer, "function");
assert.equal(typeof host.DesktopClient, "function");
const client = new host.DesktopClient();
assert.throws(() => host.nativeClientStatus(client.handle), /NotConnected/);
assert.deepEqual(client.close(), []);
assert.throws(() => host.nativeClientStatus(client.handle), /StaleHandle/);
assert.throws(() => host.initializeDesktopHost(), /already installed/);

// Generated ESM loader is deliberately imported AFTER the host selects the
// absolute wrapper path. Constructor identity proves no second GPUI addon.
// SAFETY: generated GPUIX loader is built from the source-pinned addon; identities are checked below.
const gpuix = (await import(
  pathToFileURL(resolve(import.meta.dirname, "../../toolchain/gpuix/packages/native/index.js")).href
)) as typeof import("@gpuix/native");
assert.equal(gpuix.GpuixRenderer, host.GpuixRenderer);
assert.equal(gpuix.TestGpuixRenderer, host.TestGpuixRenderer);

const renderer = new gpuix.TestGpuixRenderer(360, 160);
assert.throws(() => host.initializeDesktopHost(), /before creating a registry/);
const batch = (ops: unknown[][]) => {
  const destroyed = renderer.applyBatch(JSON.stringify(ops));
  renderer.flush();
  return destroyed;
};
batch([
  ["createElement", 1, "div"],
  ["setStyle", 1, { width: 360, height: 160, backgroundColor: "#102030" }],
  ["setRoot", 1],
  ["createElement", 2, "phux-host-probe"],
  ["setStyle", 2, { width: 260, height: 64, color: "#ffffff", backgroundColor: "#205080" }],
  ["setCustomProp", 2, "label", "native extension first"],
  ["setEventListener", 2, "click", true],
  ["appendChild", 1, 2],
]);
assert.deepEqual(renderer.getPaintedText(), ["native extension first"]);
const initial = host.desktopHostProbeCounts();
assert.equal(initial.created, 1);
assert.equal(initial.destroyed, 0);
assert.ok(initial.painted > 0);
const bounds = renderer.getElementBounds(2);
assert.ok(bounds);
assert.equal(bounds.width, 260);
assert.equal(bounds.height, 64);
renderer.simulateClick(bounds.x + 8, bounds.y + 8);
assert.ok(
  renderer.drainEvents().some((event) => event.elementId === 2 && event.eventType === "click"),
);
batch([["setCustomProp", 2, "label", "native extension updated"]]);
assert.deepEqual(renderer.getPaintedText(), ["native extension updated"]);
assert.equal(host.desktopHostProbeCounts().created, 1, "prop updates retain the native instance");
renderer.captureScreenshot(resolve(import.meta.dirname, "../../.cache/extension-smoke.png"));

batch([["destroyElement", 2]]);
assert.deepEqual(renderer.getPaintedText(), []);
assert.equal(renderer.getElementBounds(2), null);
const removed = host.desktopHostProbeCounts();
assert.equal(removed.destroyed, 1);
assert.equal(removed.dropped, 1);

// Reusing a host ID must allocate fresh native state after teardown.
batch([
  ["createElement", 2, "phux-host-probe"],
  ["setStyle", 2, { width: 260, height: 64, color: "#ffffff" }],
  ["setCustomProp", 2, "label", "native extension remounted"],
  ["appendChild", 1, 2],
]);
assert.deepEqual(renderer.getPaintedText(), ["native extension remounted"]);
assert.equal(host.desktopHostProbeCounts().created, 2);
batch([["destroyElement", 2]]);
assert.equal(host.desktopHostProbeCounts().destroyed, 2);
assert.equal(host.desktopHostProbeCounts().dropped, 2);
console.log(
  JSON.stringify({
    result: "pass",
    addon: addonPath,
    evidence:
      "release/LTO combined GPUIX+host exports, canonical loader identity, real GPU paint/bounds/events, update/unmount/remount",
    counts: host.desktopHostProbeCounts(),
  }),
);
