import assert from "node:assert/strict";
import { readFile, access, constants } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);
const read = (p) => readFile(new URL(p, root), "utf8");
const executable = (p) =>
  access(new URL(p, root), constants.X_OK).then(
    () => true,
    () => false,
  );

test("package manifest is coherent", async () => {
  const pkg = JSON.parse(await read("package.json"));
  assert.equal(pkg.name, "@phux/muse-integration");
  assert.match(pkg.version, /^\d+\.\d+\.\d+$/);
  assert.equal(pkg.scripts.test, "node --test test/*.test.mjs");
});

test("README, SKILL, and CHANGELOG exist with expected headings", async () => {
  const readme = await read("README.md");
  assert.match(readme, /^# phux for Muse/m);
  assert.match(readme, /muse-lib\.sh/);
  const skill = await read("SKILL.md");
  assert.match(skill, /^# Using phux as Muse/m);
  assert.match(skill, /finite timeout/i);
  assert.match(skill, /mspawn/);
  const changelog = await read("CHANGELOG.md");
  assert.match(changelog, /^# Changelog/m);
});

test("scripts are executable shell", async () => {
  for (const s of ["scripts/muse-lib.sh", "scripts/setup.sh", "scripts/smoke.sh"]) {
    assert.equal(await executable(s), true, `${s} must be executable`);
    const body = await read(s);
    assert.match(body, /^#!\/bin\/sh/m, `${s} needs a sh shebang`);
  }
  // setup.sh and smoke.sh execute; the lib is sourced so it must not set -u.
  for (const s of ["scripts/setup.sh", "scripts/smoke.sh"]) {
    assert.match(await read(s), /^set -u/m, `${s} needs set -u`);
  }
  assert.doesNotMatch(await read("scripts/muse-lib.sh"), /^set -u/m);
});

test("muse-lib.sh exposes the ergonomic API", async () => {
  const body = await read("scripts/muse-lib.sh");
  for (const fn of ["muse_init", "mspawn", "mrun", "msnap", "mwait", "mls", "mkill", "mdo"]) {
    assert.match(body, new RegExp(`^${fn}\\(\\)`, "m"), `muse-lib.sh must define ${fn}`);
  }
  // Verified CLI forms, not guesses: -s for new, --ephemeral flag, --yes for kill.
  assert.match(body, /phux new --json -s/);
  assert.match(body, /--ephemeral/);
  assert.match(body, /phux kill --yes/);
  // JSON parsed with jq, never shell patterns.
  assert.match(body, /jq -r/);
  assert.doesNotMatch(body, /case "\$status" in/);
});

test("setup.sh delegates to the library", async () => {
  const body = await read("scripts/setup.sh");
  assert.match(body, /\. "\$_LIBDIR\/muse-lib\.sh"/);
  assert.match(body, /muse_init/);
});

test("smoke.sh dogfoods the library", async () => {
  const body = await read("scripts/smoke.sh");
  assert.match(body, /\. "\$_LIBDIR\/muse-lib\.sh"/);
  for (const fn of ["mspawn", "mrun", "mwait", "msnap", "mkill"]) {
    assert.match(body, new RegExp(`\\b${fn}\\b`), `smoke.sh must use ${fn}`);
  }
  assert.match(body, /trap .* EXIT/, "smoke.sh must clean up on exit");
});
