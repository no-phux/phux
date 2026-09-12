#!/usr/bin/env node

import assert from "node:assert/strict";
import {
  COCKPIT_IMPORT_BASELINE_VERSION,
  isCockpitImportBaseline,
  recoveryFor,
} from "./release-drift-policy.mjs";

assert.deepEqual(recoveryFor("next"), { workflow: "next-release.yml", extraArgs: "" });
assert.deepEqual(recoveryFor("v0.27.0"), { workflow: "release.yml", extraArgs: "" });
assert.deepEqual(recoveryFor("cockpit-v0.16.2"), {
  workflow: "cockpit-release.yml",
  extraArgs: "",
});
assert.deepEqual(recoveryFor("opencode-plugin-v0.3.0"), {
  workflow: "agent-integration-release.yml",
  extraArgs: " -f dry_run=false",
});

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

process.stdout.write("release drift policy passed\n");
