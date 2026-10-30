// The Release Please settings the publish pipeline depends on, and the
// standalone lockfiles that pin root path crates at the workspace version.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Expectations } from "../lib/expect.ts";
import { ROOT } from "../lib/repo.ts";
import { table } from "../lib/value.ts";
import { syncLocks } from "./lock.ts";

/** Locks that record the root path crates outside the root workspace. */
export const STANDALONE_LOCKS = [
  "clients/phux-web/Cargo.lock",
  "clients/desktop/native/Cargo.lock",
  "clients/rune/Cargo.lock",
  "scripts/plugin-index/Cargo.lock",
];

const SETTINGS: [path: string[], expected: unknown, why: string][] = [
  [["draft"], true, "release.yml ships drafts; Release Please must create them"],
  [["force-tag-creation"], true, "a draft needs its tag before release.yml can build it"],
  [["bootstrap-sha"], undefined, "bootstrap-sha is a one-time setting"],
  [["include-component-in-tag"], false, "the root tag must be a bare vX.Y.Z"],
  [["bump-minor-pre-major"], true, "a breaking change must not bump 0.x to 1.0.0"],
  [["packages", ".", "release-type"], "simple", "the rust strategy rejects a virtual workspace"],
  [
    ["packages", ".", "package-name"],
    undefined,
    "package-name aborts root-only releases with no tag",
  ],
];

export function configViolations(config: unknown): string[] {
  const expect = new Expectations();
  for (const [path, expected, why] of SETTINGS) {
    expect.equal(
      path.reduce((value, key) => table(value)[key], config),
      expected,
      why,
    );
  }
  return expect.failures;
}

export function readConfig(root = ROOT): unknown {
  return JSON.parse(readFileSync(join(root, "release-please-config.json"), "utf8"));
}

/** Whether every standalone lock pins the root path crates at the root lock's versions. */
export function locksInStep(root = ROOT): boolean {
  return syncLocks("Cargo.lock", STANDALONE_LOCKS, true, root);
}
