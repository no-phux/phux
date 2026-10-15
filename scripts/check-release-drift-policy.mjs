#!/usr/bin/env node

import assert from "node:assert/strict";
import { status } from "./lib/output.mjs";
import {
  isArchivedRelease,
  desktopReleaseProblems,
  recoveryFor,
} from "./release-drift-policy.mjs";

assert.deepEqual(recoveryFor("next"), { workflow: "next-release.yml", extraArgs: "" });
assert.deepEqual(recoveryFor("v0.27.0"), { workflow: "publish.yml", extraArgs: "" });
assert.equal(recoveryFor("cockpit-v0.16.2"), null);
assert.deepEqual(recoveryFor("opencode-plugin-v0.3.0"), { workflow: "publish.yml", extraArgs: "" });

assert.equal(isArchivedRelease("cockpit-v0.16.1"), true);
assert.equal(isArchivedRelease("cockpit-v0.16.2-alpha.1"), true);
for (const tag of ["v0.50.0", "desktop-v0.2.0", "pi-extension-v0.3.0", "next"]) {
  assert.equal(isArchivedRelease(tag), false);
}

const desktop = {
  tag: "desktop-v0.1.0", draft: false, prerelease: false,
  assetNames: ["phux-desktop-0.1.0-macos-arm64.zip", "SHA256SUMS"],
};
assert.deepEqual(desktopReleaseProblems(desktop), []);
const desktopAlpha = {
  tag: "desktop-v0.1.0-alpha.17", draft: false, prerelease: true,
  assetNames: ["phux-desktop-0.1.0-alpha.17-macos-arm64.zip", "SHA256SUMS"],
};
// The prerelease train is retired, not stuck: no installer resolves it.
assert.deepEqual(desktopReleaseProblems(desktopAlpha), []);
assert.deepEqual(desktopReleaseProblems({ ...desktop, prerelease: true }), [
  "desktop-v0.1.0 must not be marked prerelease",
]);
assert.deepEqual(desktopReleaseProblems({ ...desktop, assetNames: ["SHA256SUMS"] }), [
  "desktop-v0.1.0 is missing phux-desktop-0.1.0-macos-arm64.zip",
]);
assert.deepEqual(desktopReleaseProblems({ ...desktop, draft: true, assetNames: [] }), []);

status("ok", "release drift policy passed");
