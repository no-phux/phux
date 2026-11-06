import { expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { list, table } from "../lib/value.ts";
import { parseLock } from "./lock.ts";
import {
  configViolations,
  expectedLockFiles,
  lockFileViolations,
  locksInStep,
  lockVersionPath,
  readConfig,
  STANDALONE_LOCKS,
  TRAINS,
  WORKSPACE_LOCKS,
} from "./release-orchestration.ts";

test("the repository's settings and locks hold", () => {
  const config = readConfig();
  expect(configViolations(config)).toEqual([]);
  expect(lockFileViolations(config)).toEqual([]);
  expect(locksInStep()).toBe(true);
});

test("the root train is the only release train", () => {
  expect(TRAINS.map(({ config }) => config)).toEqual(["release-please-config.json"]);
  const manifest = table(JSON.parse(readFileSync(join(ROOT, TRAINS[0].manifest), "utf8")));
  expect(Object.keys(manifest)).not.toContain("clients/desktop");
});

test("a release bumps every lock's workspace crates in the release commit", () => {
  const files = expectedLockFiles();
  expect(files.map(({ path }) => path)).toEqual(WORKSPACE_LOCKS);
  // The root lock records every workspace crate; each other lock some.
  for (const { jsonpath } of files) expect(jsonpath).toContain("@.name.value=='phux-protocol'");
});

test("a stale or missing lock bump is reported with its fix", () => {
  const config = table(readConfig());
  const root = table(table(config["packages"])["."]);
  const extra = list(root["extra-files"]).map(table);
  const broken = extra
    .filter(({ path }) => path !== "clients/rune/Cargo.lock")
    .map((entry) =>
      entry["path"] === "Cargo.lock" ? { ...entry, jsonpath: lockVersionPath(["phux"]) } : entry,
    );
  const failures = lockFileViolations({
    ...config,
    packages: { ...table(config["packages"]), ".": { ...root, "extra-files": broken } },
  });
  expect(failures).toHaveLength(2);
  expect(failures.join("\n")).toContain("Cargo.lock");
  expect(failures.join("\n")).toContain("clients/rune/Cargo.lock");
  expect(failures[0]).toContain("release-orchestration-check --fix");
});

test("each broken setting is reported", () => {
  const config = {
    draft: false,
    "force-tag-creation": true,
    "bootstrap-sha": "abc",
    "include-component-in-tag": false,
    "bump-minor-pre-major": true,
    packages: { ".": { "release-type": "rust", "package-name": "phux" } },
  };
  expect(configViolations(config)).toEqual([
    "release.yml ships drafts; Release Please must create them (expected true, found false)",
    'bootstrap-sha is a one-time setting (expected nothing, found "abc")',
    'the rust strategy rejects a virtual workspace (expected "simple", found "rust")',
    'package-name aborts root-only releases with no tag (expected nothing, found "phux")',
  ]);
});

test("every other lock that pins a root path crate is kept in step at release", () => {
  const read = (path: string) => parseLock(readFileSync(join(ROOT, path), "utf8"));
  const rootCrates = new Set(
    read("Cargo.lock")
      .filter((pkg) => pkg.isPath)
      .map((pkg) => pkg.name),
  );
  const locks = execFileSync("git", ["ls-files", "*Cargo.lock"], { cwd: ROOT, encoding: "utf8" })
    .split("\n")
    .filter((path) => path !== "" && path !== "Cargo.lock");
  const pinning = locks.filter((path) =>
    read(path).some((pkg) => pkg.isPath && rootCrates.has(pkg.name)),
  );
  expect(pinning.filter((path) => !STANDALONE_LOCKS.includes(path))).toEqual([]);
});
