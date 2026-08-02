import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { shouldActivatePhuxPackage } from "../src/activation.js";

const temp = mkdtempSync(join(tmpdir(), "phux-pi-activation-"));
const project = packageFixture("project", "@phux/pi");
const globalA = packageFixture("global-a", "@phux/pi");
const globalB = packageFixture("global-b", "@phux/pi");
const unrelated = packageFixture("unrelated", "other-package");

test.after(() => rmSync(temp, { recursive: true, force: true }));

test("activates the project package when no global @phux/pi is configured", () => {
  assert.equal(shouldActivatePhuxPackage(project, []), true);
  assert.equal(shouldActivatePhuxPackage(project, [
    { scope: "user", installedPath: unrelated },
  ]), true);
});

test("yields an auto-loaded project package to a globally configured @phux/pi", () => {
  assert.equal(shouldActivatePhuxPackage(project, [
    { scope: "project", installedPath: project },
    { scope: "user", installedPath: globalA },
  ]), false);
});

test("activates the globally configured package itself", () => {
  assert.equal(shouldActivatePhuxPackage(globalA, [
    { scope: "user", installedPath: globalA },
  ]), true);
});

test("uses global settings order when more than one global copy is configured", () => {
  const configured = [
    { scope: "user" as const, installedPath: globalA },
    { scope: "user" as const, installedPath: globalB },
  ];
  assert.equal(shouldActivatePhuxPackage(globalA, configured), true);
  assert.equal(shouldActivatePhuxPackage(globalB, configured), false);
});

function packageFixture(name: string, packageName: string): string {
  const root = join(temp, name);
  mkdirSync(root);
  writeFileSync(join(root, "package.json"), `${JSON.stringify({ name: packageName })}\n`);
  return root;
}
