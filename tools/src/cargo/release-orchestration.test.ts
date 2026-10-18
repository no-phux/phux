import { describe, expect, test } from "bun:test";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import {
  configViolations,
  locksInStep,
  readConfig,
  STANDALONE_LOCKS,
} from "./release-orchestration.ts";

describe("release orchestration", () => {
  test("the repository's Release Please settings hold", () => {
    expect(configViolations(readConfig())).toEqual([]);
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
      "publish.yml ships drafts; Release Please must create them (expected true, found false)",
      'bootstrap-sha is a one-time setting (expected nothing, found "abc")',
      'the rust strategy rejects a virtual workspace (expected "simple", found "rust")',
      'package-name aborts root-only releases with no tag (expected nothing, found "phux")',
    ]);
  });

  test("a standalone lock behind the root workspace fails", () => {
    const dir = mkdtempSync(join(tmpdir(), "orchestration-"));
    for (const path of ["Cargo.lock", ...STANDALONE_LOCKS]) {
      mkdirSync(dirname(join(dir, path)), { recursive: true });
      cpSync(join(ROOT, path), join(dir, path));
    }
    expect(locksInStep(dir)).toBe(true);
    const desktop = join(dir, "clients/desktop/native/Cargo.lock");
    const version = /^name = "phux-protocol"\nversion = "[^"]+"$/m;
    writeFileSync(
      desktop,
      readFileSync(desktop, "utf8").replace(version, 'name = "phux-protocol"\nversion = "0.0.1"'),
    );
    expect(locksInStep(dir)).toBe(false);
  });
});
