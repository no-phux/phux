import { expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { parseLock } from "./lock.ts";
import {
  configViolations,
  locksInStep,
  readConfig,
  STANDALONE_LOCKS,
} from "./release-orchestration.ts";

test("the repository's settings and standalone locks hold", () => {
  expect(configViolations(readConfig())).toEqual([]);
  expect(locksInStep()).toBe(true);
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
