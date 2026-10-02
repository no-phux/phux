#!/usr/bin/env node

import assert from "node:assert/strict";
import {
  COCKPIT_IMPORT_BASELINE_VERSION,
  isCockpitImportBaseline,
  isUnreleasedDesktop,
  desktopReleaseProblems,
  recoveryFor,
} from "./release-drift-policy.mjs";

assert.deepEqual(recoveryFor("next"), { workflow: "next-release.yml", extraArgs: "" });
assert.deepEqual(recoveryFor("v0.27.0"), { workflow: "publish.yml", extraArgs: "" });
assert.deepEqual(recoveryFor("cockpit-v0.16.2"), { workflow: "publish.yml", extraArgs: "" });
assert.deepEqual(recoveryFor("opencode-plugin-v0.3.0"), { workflow: "publish.yml", extraArgs: "" });

const historyTip = "filtered-cockpit-tip";
const baseline = {
  path: "clients/cockpit",
  version: COCKPIT_IMPORT_BASELINE_VERSION,
  bootstrapSha: historyTip,
  historyTip,
};
assert.equal(isCockpitImportBaseline(baseline), true);
assert.equal(isCockpitImportBaseline({ ...baseline, version: "0.16.2" }), false);
assert.equal(isCockpitImportBaseline({ ...baseline, path: "." }), false);
assert.equal(isCockpitImportBaseline({ ...baseline, bootstrapSha: undefined }), false);
assert.equal(isCockpitImportBaseline({ ...baseline, bootstrapSha: "other-tip" }), false);

const unreleased = { path: "clients/desktop", version: "0.0.0", initialVersion: "0.1.0-alpha.1" };
assert.equal(isUnreleasedDesktop(unreleased), true);
assert.equal(isUnreleasedDesktop({ ...unreleased, version: "0.1.0-alpha.1" }), false);
assert.equal(isUnreleasedDesktop({ ...unreleased, initialVersion: undefined }), false);
assert.equal(isUnreleasedDesktop({ ...unreleased, path: "." }), false);
const desktop = {
  tag: "desktop-v0.1.0-alpha.1", draft: false, prerelease: true,
  assetNames: ["phux-desktop-0.1.0-alpha.1-macos-arm64.zip", "SHA256SUMS"],
};
assert.deepEqual(desktopReleaseProblems(desktop), []);
assert.deepEqual(desktopReleaseProblems({ ...desktop, prerelease: false }), [
  "desktop-v0.1.0-alpha.1 must be marked prerelease",
]);
assert.deepEqual(desktopReleaseProblems({ ...desktop, assetNames: ["SHA256SUMS"] }), [
  "desktop-v0.1.0-alpha.1 is missing phux-desktop-0.1.0-alpha.1-macos-arm64.zip",
]);
assert.deepEqual(desktopReleaseProblems({ ...desktop, draft: true, assetNames: [] }), []);

process.stdout.write("release drift policy passed\n");
