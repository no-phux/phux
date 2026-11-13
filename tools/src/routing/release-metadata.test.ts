import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  manifestOnly,
  releaseMetadataOnly,
  skillOnly,
  workspaceLockOnly,
  workspaceManifestOnly,
} from "./release-metadata.ts";
import { WORKSPACE_LOCKS } from "../cargo/release-orchestration.ts";
import { ROOT } from "../lib/repo.ts";

const REGISTRY = 'source = "registry+https://github.com/rust-lang/crates.io-index"\n';
const MANIFEST = '[workspace.package]\nversion = "0.1.0"\n[workspace.dependencies]\nserde = "1"\n';
const LOCK =
  'version = 3\n\n[[package]]\nname = "phux"\nversion = "0.1.0"\n' +
  `\n[[package]]\nname = "registry-crate"\nversion = "0.1.0"\n${REGISTRY}checksum = "unchanged"\n`;
const ALPHA_LOCK = `${LOCK}\n[[package]]\nname = "phux-rune"\nversion = "0.1.0-alpha.10"\n`;
const SKILL = '  version: "0.1.0" # x-release-please-version\nUse phux when needed.\n';

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
  [
    "lock keeps an unmoved alpha path crate",
    workspaceLockOnly(ALPHA_LOCK, ALPHA_LOCK.replace('"0.1.0"', '"0.2.0"')),
    true,
  ],
  [
    "lock moves a path crate to an alpha",
    workspaceLockOnly(LOCK, LOCK.replace('"0.1.0"', '"0.2.0-alpha.1"')),
    false,
  ],
  ["skill version bump", skillOnly(SKILL, SKILL.replace("0.1.0", "0.2.0")), true],
  ["skill body edit", skillOnly(SKILL, SKILL.replace("Use phux", "Use something else")), false],
  [
    "manifest package bump",
    manifestOnly('{"integrations/pi":"0.1.0"}', '{"integrations/pi":"0.2.0"}'),
    true,
  ],
  [
    "manifest package null",
    manifestOnly('{"integrations/pi":"0.1.0"}', '{"integrations/pi":null}'),
    false,
  ],
  ["root alpha", manifestOnly('{".":"0.1.0"}', '{".":"0.1.0-alpha.1"}'), false],
])("%s", (_, actual, expected) => {
  expect(actual).toBe(expected);
});

test("an unknown path, a missing side or an empty diff fails closed", () => {
  const pair = [MANIFEST, MANIFEST.replace("0.1.0", "0.2.0")] as const;
  const bumpedLock = LOCK.replace("0.1.0", "0.2.0");
  const release = ["Cargo.toml", ...WORKSPACE_LOCKS];
  const read = (path: string) => (path === "Cargo.toml" ? pair : ([LOCK, bumpedLock] as const));
  expect(releaseMetadataOnly(release, read)).toBe(true);
  // Every lock moves with the workspace version, or --locked builds break.
  expect(releaseMetadataOnly(release.slice(0, -1), read)).toBe(false);
  expect(releaseMetadataOnly(["Cargo.toml"], () => pair)).toBe(false);
  expect(
    releaseMetadataOnly(release, (path) =>
      path === "Cargo.toml" ? [MANIFEST, MANIFEST.replace('"1"', '"2"')] : read(path),
    ),
  ).toBe(false);
  expect(releaseMetadataOnly(["crates/phux/src/main.rs"], () => pair)).toBe(false);
  expect(releaseMetadataOnly(["Cargo.toml"], () => undefined)).toBe(false);
  expect(releaseMetadataOnly(["Cargo.toml"], () => [undefined, pair[1]])).toBe(false);
  expect(releaseMetadataOnly([], () => pair)).toBe(false);
});

test.each(["pi", "opencode-v2", "omp", "grok", "claude"])(
  "%s release package allows only its top-level version to change",
  (integration) => {
    const path = `integrations/${integration}/package.json`;
    const before = readFileSync(join(ROOT, path), "utf8");
    const document = JSON.parse(before) as Record<string, unknown>;
    const version = String(document["version"]).replace(/\d+$/, (patch) =>
      String(Number(patch) + 1),
    );
    const after: Record<string, unknown> = { ...document, version };
    expect(releaseMetadataOnly([path], () => [before, JSON.stringify(after)])).toBe(true);
    // Read both full documents: nested dependency versions, scripts and runtime
    // entrypoints must not disappear behind the package's release version.
    for (const [key, value] of Object.entries({
      name: "@phux/other",
      dependencies: { injected: "1.0.0" },
      devDependencies: { injected: "1.0.0" },
      peerDependencies: { injected: "1.0.0" },
      overrides: { injected: "1.0.0" },
      scripts: { build: "run-something-else" },
      exports: { ".": "./other.js" },
      main: "./other.js",
      files: ["other.js"],
      type: "commonjs",
    })) {
      const changed = JSON.stringify({ ...after, [key]: value });
      expect(releaseMetadataOnly([path], () => [before, changed])).toBe(false);
      expect(releaseMetadataOnly([path], () => [changed, before])).toBe(false);
    }
    const withoutScripts = { ...after };
    delete withoutScripts["scripts"];
    expect(releaseMetadataOnly([path], () => [before, JSON.stringify(withoutScripts)])).toBe(false);
    expect(releaseMetadataOnly([path], () => [undefined, JSON.stringify(after)])).toBe(false);
    expect(releaseMetadataOnly([path], () => [before, undefined])).toBe(false);
  },
);

test.each([
  "{",
  "null",
  "[]",
  '[{"version":"0.9.2"}]',
  "{}",
  '{"version":null}',
  '{"version":9}',
  '{"version":"0.9"}',
  '{"version":"v0.9.2"}',
  '{"version":"0.9.2-alpha.1"}',
  '{"version":"0.9.2+build"}',
  '{"nested":{"version":"0.9.2"}}',
])("invalid integration manifest fails closed: %s", (invalid) => {
  const path = "integrations/opencode-v2/package.json";
  const valid = '{"version":"0.9.1"}';
  expect(releaseMetadataOnly([path], () => [valid, invalid])).toBe(false);
  expect(releaseMetadataOnly([path], () => [invalid, valid])).toBe(false);
});

test.each([
  "package.json",
  "tools/package.json",
  "integrations/runtime/package.json",
  "integrations/muse/package.json",
  "integrations/new-package/package.json",
  "integrations/opencode-v2/nested/package.json",
  "integrations/opencode-v2/package-lock.json",
  "integrations/opencode-v2/bun.lock",
])("unlisted package or lock keeps normal routing: %s", (path) => {
  expect(releaseMetadataOnly([path], () => ['{"version":"0.9.1"}', '{"version":"0.9.2"}'])).toBe(
    false,
  );
});
