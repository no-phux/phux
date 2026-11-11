import { expect, test } from "bun:test";
import {
  manifestOnly,
  releaseMetadataOnly,
  skillOnly,
  workspaceLockOnly,
  workspaceManifestOnly,
} from "./release-metadata.ts";
import { WORKSPACE_LOCKS } from "../cargo/release-orchestration.ts";

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
