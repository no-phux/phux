#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const read = (path) => readFile(join(root, path), "utf8");

const [configText, manifestText, workflow, linearWorkflow, cockpitVersionText] = await Promise.all([
  read("release-please-config.json"),
  read(".release-please-manifest.json"),
  read(".github/workflows/release-please.yml"),
  read(".github/workflows/linear-release.yml"),
  read("clients/cockpit/version.txt"),
]);
const config = JSON.parse(configText);
const manifest = JSON.parse(manifestText);
const cockpitVersion = cockpitVersionText.trim();
const cockpit = config.packages?.["clients/cockpit"];

assert.equal(config["bootstrap-sha"], undefined, "bootstrap-sha must stay removed after the first canonical release");
assert.equal(config["force-tag-creation"], true, "draft releases require release-please-owned tags");
assert.equal(config.draft, true, "artifact workflows require private draft releases");
assert.equal(cockpit?.component, "cockpit");
assert.equal(cockpit?.["include-component-in-tag"], true);
assert.equal(cockpit?.["include-v-in-tag"], true);
assert.equal(cockpit?.["bootstrap-sha"], undefined, "bootstrap-sha is a top-level-only option");
assert.equal(manifest["clients/cockpit"], cockpitVersion, "Cockpit manifest and source version must agree");

assert.match(
  workflow,
  /cockpit_release_created: \$\{\{ steps\.rp\.outputs\['clients\/cockpit--release_created'\] \}\}/,
  "release-please path output must preserve clients/cockpit",
);
assert.match(
  workflow,
  /cockpit_tag_name: \$\{\{ steps\.rp\.outputs\['clients\/cockpit--tag_name'\] \}\}/,
  "Cockpit tag output must preserve clients/cockpit",
);
assert.doesNotMatch(workflow, /clients--cockpit--/, "slash-normalized release-please outputs do not exist");
assert.doesNotMatch(workflow, /\bgit tag\b/, "release-please is the sole tag owner");

assert.ok(
  linearWorkflow.includes("name: ${{ inputs.tag }}"),
  "Linear continuous pipelines name the SHA unless name is the tag",
);
assert.match(linearWorkflow, /extract_changelog_section\.py/, "Linear notes come from the tagged changelog");
assert.match(workflow, /cockpit-linear-release-sync:/, "Cockpit tags must be reported to Linear");
assert.match(workflow, /cockpit-linear-release-promote:/, "Cockpit Linear releases must be promoted after artifacts");
assert.match(linearWorkflow, /LINEAR_COCKPIT_RELEASE_ACCESS_KEY/, "Cockpit uses its own Linear pipeline key");
assert.match(linearWorkflow, /include_paths: clients\/cockpit\/\*\*/, "Cockpit Linear scans only clients/cockpit");

process.stdout.write("release orchestration passed\n");
