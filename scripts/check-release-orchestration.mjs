#!/usr/bin/env node

// The Release Please settings the publish pipeline depends on, and the
// standalone lockfiles that pin root path crates at the workspace version.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { status } from "./lib/output.mjs";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const config = JSON.parse(readFileSync(join(root, "release-please-config.json"), "utf8"));

assert.equal(config.draft, true, "publish.yml ships drafts; Release Please must create them");
assert.equal(config["force-tag-creation"], true, "a draft needs its tag before publish.yml can build it");
assert.equal(config["bootstrap-sha"], undefined, "bootstrap-sha is a one-time setting");

execFileSync("python3", [
  "scripts/sync-path-lock-versions.py", "--check", "Cargo.lock",
  "clients/phux-web/Cargo.lock", "clients/desktop/native/Cargo.lock",
], { cwd: root, stdio: "inherit" });

status("ok", "release orchestration passed");
