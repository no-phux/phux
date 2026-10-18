import { expect, test } from "bun:test";
import {
  manifestOnly,
  releaseMetadataOnly,
  skillOnly,
  workspaceLockOnly,
  workspaceManifestOnly,
} from "./release-metadata.ts";

test("a dependency edit in the workspace manifest still compiles", () => {
  const before = '[workspace.package]\nversion = "0.1.0"\n[workspace.dependencies]\nserde = "1"\n';
  const after = before.replace('serde = "1"', 'serde = "2"');
  expect(workspaceManifestOnly(before, after)).toBe(false);
  expect(releaseMetadataOnly(["Cargo.toml"], () => [before, after])).toBe(false);
  expect(workspaceManifestOnly(before, before.replace("0.1.0", "0.2.0"))).toBe(true);
});

test("a lockfile checksum edit still compiles", () => {
  const before =
    'version = 3\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\n' +
    'source = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "original"\n';
  expect(workspaceLockOnly(before, before.replace("original", "changed"))).toBe(false);
});

test("a path version bump does not exempt registry version changes", () => {
  const before =
    'version = 3\n\n[[package]]\nname = "phux"\nversion = "0.1.0"\n' +
    '\n[[package]]\nname = "registry-crate"\nversion = "0.1.0"\n' +
    'source = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "unchanged"\n';
  const pathBump = before.replace('version = "0.1.0"', 'version = "0.2.0"');
  expect(workspaceLockOnly(before, pathBump)).toBe(true);
  expect(
    workspaceLockOnly(before, pathBump.replace('version = "0.1.0"', 'version = "0.2.0"')),
  ).toBe(false);
});

test("a skill body edit still compiles", () => {
  const before = '  version: "0.1.0" # x-release-please-version\nUse phux when needed.\n';
  expect(skillOnly(before, before.replace("Use phux", "Use something else"))).toBe(false);
  expect(skillOnly(before, before.replace("0.1.0", "0.2.0"))).toBe(true);
});

test("a desktop release skips compiles but a dependency edit does not", () => {
  const pkg = { version: "0.1.0", dependencies: { "@gpuix/native": "0.10.0" } };
  const bumped = { ...pkg, version: "0.2.0" };
  const pairs = new Map<string, readonly [string, string]>([
    ["clients/desktop/package.json", [JSON.stringify(pkg), JSON.stringify(bumped)]],
    [
      ".release-please-manifest.json",
      [
        JSON.stringify({ "clients/desktop": "0.1.0" }),
        JSON.stringify({ "clients/desktop": "0.2.0" }),
      ],
    ],
  ]);
  expect(releaseMetadataOnly([...pairs.keys()], (path) => pairs.get(path))).toBe(true);
  const moved = { ...bumped, dependencies: { "@gpuix/native": "0.11.0" } };
  pairs.set("clients/desktop/package.json", [JSON.stringify(pkg), JSON.stringify(moved)]);
  expect(releaseMetadataOnly([...pairs.keys()], (path) => pairs.get(path))).toBe(false);
});

test("the release manifest accepts stable and alpha desktop versions only", () => {
  const before = JSON.stringify({ "clients/desktop": "0.0.0" });
  expect(manifestOnly(before, JSON.stringify({ "clients/desktop": "0.1.0" }))).toBe(true);
  expect(
    manifestOnly(
      JSON.stringify({ "clients/desktop": "0.1.0-alpha.17" }),
      JSON.stringify({ "clients/desktop": "0.1.0" }),
    ),
  ).toBe(true);
  for (const version of ["0.1.0-alpha.01", "0.1.0-alpha.0", "not-a-version", null]) {
    expect(manifestOnly(before, JSON.stringify({ "clients/desktop": version }))).toBe(false);
  }
  expect(
    manifestOnly(JSON.stringify({ ".": "0.1.0" }), JSON.stringify({ ".": "0.1.0-alpha.1" })),
  ).toBe(false);
});

test("an unknown path, a missing side or an empty diff fails closed", () => {
  const pair = ['version = "0.1.0"\n', 'version = "0.2.0"\n'] as const;
  expect(releaseMetadataOnly(["crates/phux/src/main.rs"], () => pair)).toBe(false);
  expect(releaseMetadataOnly(["Cargo.toml"], () => undefined)).toBe(false);
  expect(releaseMetadataOnly(["Cargo.toml"], () => [undefined, pair[1]])).toBe(false);
  expect(releaseMetadataOnly([], () => pair)).toBe(false);
});
