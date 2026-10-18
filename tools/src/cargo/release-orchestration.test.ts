import { expect, test } from "bun:test";
import { configViolations, locksInStep, readConfig } from "./release-orchestration.ts";

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
