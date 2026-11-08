// The Release Please settings the publish pipeline depends on, and the
// lockfiles that pin root path crates at the workspace version.
//
// One train: the root config/manifest pair releases the CLI and the
// integrations. Phux Rune tags its own releases outside Release Please.
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { Expectations } from "../lib/expect.ts";
import { ROOT } from "../lib/repo.ts";
import { list, str, table } from "../lib/value.ts";
import { parseLock, syncLocks } from "./lock.ts";

export const ROOT_TRAIN = {
  config: "release-please-config.json",
  manifest: ".release-please-manifest.json",
} as const;
export const TRAINS = [ROOT_TRAIN] as const;

/** Locks that record the root path crates outside the root workspace. */
export const STANDALONE_LOCKS = [
  "clients/phux-web/Cargo.lock",
  "clients/rune/Cargo.lock",
  "crates/phux-server/tests/fixtures/host-only-engine/Cargo.lock",
  "scripts/plugin-index/Cargo.lock",
];

/** Every lock a root release rewrites, in the release commit itself. */
export const WORKSPACE_LOCKS = ["Cargo.lock", ...STANDALONE_LOCKS];

type Setting = [path: string[], expected: unknown, why: string];

const SHARED: Setting[] = [
  [["draft"], true, "release.yml ships drafts; Release Please must create them"],
  [["force-tag-creation"], true, "a draft needs its tag before release.yml can build it"],
  [["bootstrap-sha"], undefined, "bootstrap-sha is a one-time setting"],
];

const ROOT_SETTINGS: Setting[] = [
  ...SHARED,
  [["include-component-in-tag"], false, "the root tag must be a bare vX.Y.Z"],
  [["bump-minor-pre-major"], true, "a breaking change must not bump 0.x to 1.0.0"],
  [["packages", ".", "release-type"], "simple", "the rust strategy rejects a virtual workspace"],
  [
    ["packages", ".", "package-name"],
    undefined,
    "package-name aborts root-only releases with no tag",
  ],
];

function check(config: unknown, settings: Setting[]): string[] {
  const expect = new Expectations();
  for (const [path, expected, why] of settings) {
    expect.equal(
      path.reduce((value, key) => table(value)[key], config),
      expected,
      why,
    );
  }
  return expect.failures;
}

export function configViolations(config: unknown): string[] {
  return check(config, ROOT_SETTINGS);
}

export function readConfig(root = ROOT, file: string = ROOT_TRAIN.config): unknown {
  return JSON.parse(readFileSync(join(root, file), "utf8"));
}

/** Whether every standalone lock pins the root path crates at the root lock's versions. */
export function locksInStep(root = ROOT): boolean {
  return syncLocks("Cargo.lock", STANDALONE_LOCKS, true, root);
}

/**
 * The extra-files jsonpath that makes Release Please bump `names` in a
 * Cargo.lock. Its toml updater wraps every parsed value as
 * `{start, end, value}`, hence `@.name.value`.
 */
export function lockVersionPath(names: readonly string[]): string {
  const match = names.map((name) => `@.name.value=='${name}'`).join(" || ");
  return `$.package[?(!@.source && (${match}))].version`;
}

function workspaceVersion(root: string): string | undefined {
  const manifest = Bun.TOML.parse(readFileSync(join(root, "Cargo.toml"), "utf8"));
  return str(table(table(table(manifest)["workspace"])["package"])["version"]);
}

/** The path crates `lock` pins at the workspace version: what a release must bump. */
export function workspaceCrates(lock: string, version: string, root = ROOT): string[] {
  return parseLock(readFileSync(join(root, lock), "utf8"))
    .filter((pkg) => pkg.isPath && pkg.version === version)
    .map((pkg) => pkg.name)
    .toSorted();
}

/** The root package's lock extra-files, expected for the tree under `root`. */
export function expectedLockFiles(root = ROOT): { type: string; path: string; jsonpath: string }[] {
  const version = workspaceVersion(root) ?? "";
  return WORKSPACE_LOCKS.map((path) => ({
    type: "toml",
    path,
    jsonpath: lockVersionPath(workspaceCrates(path, version, root)),
  }));
}

/**
 * Release Please must rewrite every lock entry the workspace version bump
 * moves, in the release commit: a lock fixed up afterwards races a merge.
 */
export function lockFileViolations(config: unknown, root = ROOT): string[] {
  const extra = list(table(table(table(config)["packages"])["."])["extra-files"]).map(table);
  const failures: string[] = [];
  for (const want of expectedLockFiles(root)) {
    const found = extra.find((entry) => entry["path"] === want.path);
    if (found?.["type"] === "toml" && found["jsonpath"] === want.jsonpath) continue;
    failures.push(
      `${ROOT_TRAIN.config}: the "." extra-files entry for ${want.path} must be ` +
        `${JSON.stringify(want)} so a release bumps its workspace crates ` +
        "(regenerate: bun tools/main.ts release-orchestration-check --fix)",
    );
  }
  return failures;
}

/** Rewrite the root package's lock extra-files to match the tree, keeping every other entry. */
export function writeLockFiles(root = ROOT): void {
  const path = join(root, ROOT_TRAIN.config);
  const config = table(JSON.parse(readFileSync(path, "utf8")));
  const pkg = table(table(config["packages"])["."]);
  const locks = new Set<unknown>(WORKSPACE_LOCKS);
  const kept = list(pkg["extra-files"]).filter((entry) => !locks.has(table(entry)["path"]));
  pkg["extra-files"] = [...kept, ...expectedLockFiles(root)];
  writeFileSync(path, `${JSON.stringify(config, null, 2)}\n`);
}
