import assert from "node:assert/strict";
import { readFile, access, constants } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);
const read = (p) => readFile(new URL(p, root), "utf8");
const readAgentLib = (p) => readFile(new URL(`../../.agents/${p}`, root), "utf8");
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
  assert.match(readme, /phux-agent\.sh/);
  const skill = await read("SKILL.md");
  assert.match(skill, /^# Using phux as Muse/m);
  assert.match(skill, /finite timeout/i);
  assert.match(skill, /phux_spawn/);
  const changelog = await read("CHANGELOG.md");
  assert.match(changelog, /^# Changelog/m);
});

test("general agent library exists and is harness-agnostic", async () => {
  const body = await readAgentLib("lib/phux-agent.sh");
  assert.match(body, /^#!\/bin\/sh/m);
  // No Muse coupling: generic phux_ prefix, no m* aliases.
  for (const fn of ["phux_init", "phux_spawn", "phux_run", "phux_snap", "phux_wait", "phux_ls", "phux_kill", "phux_do"]) {
    assert.match(body, new RegExp(`^${fn}\\(\\)`, "m"), `lib must define ${fn}`);
  }
  assert.doesNotMatch(body, /^m(spawn|run|snap|wait|ls|kill|do)\(\)/m);
  assert.doesNotMatch(body, /MUSE_DEFAULT_TIMEOUT/);
  // Verified CLI forms, not guesses.
  assert.match(body, /_phux_json new --json -s/);
  assert.match(body, /--ephemeral/);
  assert.match(body, /_phux_json kill --yes/);
  // JSON parsed with jq, never shell patterns. Documents the no_server contract.
  assert.match(body, /jq -[a-z]*r/);
  assert.match(body, /no_server/);
});

test("scripts are executable and delegate to the general library", async () => {
  for (const s of ["scripts/setup.sh", "scripts/smoke.sh"]) {
    assert.equal(await executable(s), true, `${s} must be executable`);
    const body = await read(s);
    assert.match(body, /^#!\/bin\/sh/m, `${s} needs a sh shebang`);
    assert.match(body, /^set -u/m, `${s} needs set -u`);
    assert.match(body, /\. "\$_LIBDIR\/\.\.\/\.\.\/\.\.\/\.agents\/lib\/phux-agent\.sh"/);
  }
  // setup.sh uses phux_init; smoke.sh dogfoods the drive cycle.
  assert.match(await read("scripts/setup.sh"), /\bphux_init\b/);
  const smoke = await read("scripts/smoke.sh");
  for (const fn of ["phux_spawn", "phux_run", "phux_wait", "phux_snap", "phux_kill"]) {
    assert.match(smoke, new RegExp(`\\b${fn}\\b`), `smoke.sh must use ${fn}`);
  }
  assert.match(smoke, /trap .* 0$/m, "smoke.sh must clean up on exit");
});
