import { expect, test } from "bun:test";
import {
  manifestOnly,
  releaseMetadataOnly,
  skillOnly,
  workspaceLockOnly,
  workspaceManifestOnly,
} from "./release-metadata.ts";

const REGISTRY = 'source = "registry+https://github.com/rust-lang/crates.io-index"\n';
const MANIFEST = '[workspace.package]\nversion = "0.1.0"\n[workspace.dependencies]\nserde = "1"\n';
const LOCK =
  'version = 3\n\n[[package]]\nname = "phux"\nversion = "0.1.0"\n' +
  `\n[[package]]\nname = "registry-crate"\nversion = "0.1.0"\n${REGISTRY}checksum = "unchanged"\n`;
const SKILL = '  version: "0.1.0" # x-release-please-version\nUse phux when needed.\n';
const desktop = (before: unknown, after: unknown) =>
  manifestOnly(
    JSON.stringify({ "clients/desktop": before }),
    JSON.stringify({ "clients/desktop": after }),
  );

test.each<[string, boolean, boolean]>([
  [
    "manifest version bump",
    workspaceManifestOnly(MANIFEST, MANIFEST.replace("0.1.0", "0.2.0")),
    true,
  ],
  [
    "manifest dependency edit",
    workspaceManifestOnly(MANIFEST, MANIFEST.replace('"1"', '"2"')),
    false,
  ],
  ["lock path version bump", workspaceLockOnly(LOCK, LOCK.replace("0.1.0", "0.2.0")), true],
  ["lock registry version bump", workspaceLockOnly(LOCK, LOCK.replaceAll("0.1.0", "0.2.0")), false],
  ["lock checksum edit", workspaceLockOnly(LOCK, LOCK.replace("unchanged", "changed")), false],
  ["skill version bump", skillOnly(SKILL, SKILL.replace("0.1.0", "0.2.0")), true],
  ["skill body edit", skillOnly(SKILL, SKILL.replace("Use phux", "Use something else")), false],
  ["desktop first stable", desktop("0.0.0", "0.1.0"), true],
  ["desktop alpha graduates", desktop("0.1.0-alpha.17", "0.1.0"), true],
  ["desktop leading-zero alpha", desktop("0.0.0", "0.1.0-alpha.01"), false],
  ["desktop alpha zero", desktop("0.0.0", "0.1.0-alpha.0"), false],
  ["desktop non-version", desktop("0.0.0", "not-a-version"), false],
  ["desktop null", desktop("0.0.0", null), false],
  ["root alpha", manifestOnly('{".":"0.1.0"}', '{".":"0.1.0-alpha.1"}'), false],
])("%s", (_, actual, expected) => {
  expect(actual).toBe(expected);
});

test("a desktop release skips compiles but a dependency edit does not", () => {
  const pkg = { version: "0.1.0", dependencies: { "@gpuix/native": "0.10.0" } };
  const bumped = { ...pkg, version: "0.2.0" };
  const moved = { ...bumped, dependencies: { "@gpuix/native": "0.11.0" } };
  const releasePlease = [
    JSON.stringify({ "clients/desktop": "0.1.0" }),
    JSON.stringify({ "clients/desktop": "0.2.0" }),
  ] as const;
  const paths = ["clients/desktop/package.json", ".release-please-manifest.json"];
  for (const [after, want] of [
    [bumped, true],
    [moved, false],
  ] as const) {
    const read = (path: string) =>
      path === paths[0] ? ([JSON.stringify(pkg), JSON.stringify(after)] as const) : releasePlease;
    expect(releaseMetadataOnly(paths, read)).toBe(want);
  }
});

test("an unknown path, a missing side or an empty diff fails closed", () => {
  const pair = [MANIFEST, MANIFEST.replace("0.1.0", "0.2.0")] as const;
  expect(releaseMetadataOnly(["Cargo.toml"], () => pair)).toBe(true);
  expect(
    releaseMetadataOnly(["Cargo.toml"], () => [MANIFEST, MANIFEST.replace('"1"', '"2"')]),
  ).toBe(false);
  expect(releaseMetadataOnly(["crates/phux/src/main.rs"], () => pair)).toBe(false);
  expect(releaseMetadataOnly(["Cargo.toml"], () => undefined)).toBe(false);
  expect(releaseMetadataOnly(["Cargo.toml"], () => [undefined, pair[1]])).toBe(false);
  expect(releaseMetadataOnly([], () => pair)).toBe(false);
});
