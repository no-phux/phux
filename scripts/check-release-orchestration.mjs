#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const read = (path) => readFile(join(root, path), "utf8");

const [configText, manifestText, releasePlease, publish, linearWorkflow, integrationWorkflow, cockpitVersionText] = await Promise.all([
  read("release-please-config.json"),
  read(".release-please-manifest.json"),
  read(".github/workflows/release-please.yml"),
  read(".github/workflows/publish.yml"),
  read(".github/workflows/linear-release.yml"),
  read(".github/workflows/agent-integration-release.yml"),
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

assert.doesNotMatch(releasePlease, /clients--cockpit--/, "slash-normalized release-please outputs do not exist");
assert.doesNotMatch(releasePlease, /\bgit tag\b/, "release-please is the sole tag owner");
assert.doesNotMatch(releasePlease, /wait_validation\.py/, "release-please must not poll ci");
assert.doesNotMatch(releasePlease, /uses: \.\/\.github\/workflows\/release\.yml/, "publish.yml owns artifact fan-out");

assert.match(publish, /uses: \.\/\.github\/workflows\/release\.yml/, "publish ships root releases");
assert.match(publish, /uses: \.\/\.github\/workflows\/cockpit-release\.yml/, "publish ships Cockpit");
assert.match(publish, /ffi-android\.yml/, "root releases attach the Android UniFFI zip");
assert.match(publish, /ffi-xcframework\.yml/, "root releases attach the xcframework");
assert.match(publish, /linear-release\.yml/, "published releases are reported to Linear");
assert.match(publish, /publish_plan\.py/, "one plan decides every component");
assert.doesNotMatch(
  publish,
  /uses: \.\/\.github\/workflows\/agent-integration-release\.yml/,
  "npm trusts agent-integration-release.yml as the entry workflow; workflow_call publishes as publish.yml and npm returns 404",
);
assert.match(
  publish,
  /python3 scripts\/ci\/dispatch_integration_publishes\.py/,
  "publish dispatches the integration workflow so the OIDC filename matches npm",
);
assert.doesNotMatch(
  integrationWorkflow,
  /workflow_call:/,
  "the integration publisher must be the entry workflow",
);

assert.ok(
  linearWorkflow.includes("name: ${{ inputs.tag }}"),
  "Linear continuous pipelines name the SHA unless name is the tag",
);
assert.match(linearWorkflow, /extract_changelog_section\.py/, "Linear notes come from the tagged changelog");
assert.match(linearWorkflow, /LINEAR_COCKPIT_RELEASE_ACCESS_KEY/, "Cockpit uses its own Linear pipeline key");
assert.match(linearWorkflow, /include_paths: clients\/cockpit\/\*\*/, "Cockpit Linear scans only clients/cockpit");

process.stdout.write("release orchestration passed\n");
