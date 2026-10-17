---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-16
---

# Releasing

**TL;DR.** Release Please cuts versions, tags, and private drafts. `publish`
ships every draft whose tagged commit has a green `ci`, then rebuilds
`channels/stable.json`. `next-release` ships green `main` to the moving
`next` prerelease and `channels/next.json`. One dispatch of `publish`
finishes a stuck draft. `phux-protocol` on crates.io stays a human dispatch.
The decisions are [ADR-0176](./adr/0176-the-release-pipeline.md). CI lanes
are in [CI](./CI.md).

## What ships

| Product | Tag | Assets | Built by |
|---|---|---|---|
| `phux` + `phux-mcp` | `vX.Y.Z` | `phux-vX.Y.Z-<target>.tar.gz` + `.sha256` | `release.yml` -> `build-cli.yml` |
| Phux Desktop | `desktop-vX.Y.Z` | `phux-desktop-X.Y.Z-macos-arm64.zip` + `.sha256`, `SHA256SUMS` | `desktop-release.yml` |
| Mobile FFI | on `vX.Y.Z` | `PhuxFFI-*`, `PhuxMobileFFI-*` zips + `.sha256` + `.provenance` | `ffi-xcframework.yml`, `ffi-android.yml` |
| `@phux/pi`, `@phux/opencode`, `@phux/omp` | `<component>-vX.Y.Z` | npm with provenance + release tarball | `agent-integration-release.yml` |
| Claude and Grok plugins | `claude-plugin-vX.Y.Z`, `grok-plugin-vX.Y.Z` | release tarball; repository marketplace | `agent-integration-release.yml` |
| `phux-protocol` | `vX.Y.Z` | crates.io | `publish-crate.yml`, by hand |
| Next | `next` | `phux-next.<sha>-<target>.tar.gz`, `phux-desktop-next.<sha>-macos-arm64.zip`, sidecars | `next-release.yml` |
| Manifests | `channels` | `stable.json`, `next.json` | `scripts/ci/release_manifest.py` |

CLI targets are `aarch64-apple-darwin` (`xcode-27`), `x86_64-unknown-linux-gnu`
(`ubuntu-24.04`), and `aarch64-unknown-linux-gnu` (`ubuntu-24.04-arm`). Linux
builds run in an `ubuntu:22.04` container to link against glibc 2.35. Desktop
is Apple silicon on macOS 27 or later, ad-hoc signed and not notarized. Only
CLI releases are GitHub's Latest. The Homebrew tap
([no-phux/homebrew-tap](https://github.com/no-phux/homebrew-tap)) renders
`Formula/phux.rb` from Latest every fifteen minutes.

## Control surface

| You want to | Do this |
|---|---|
| Ship a release | Let the release PR auto-merge once `ci` is green, or mark it ready and squash-merge it |
| Finish or rebuild a draft | `gh workflow run publish.yml -f tag=<tag>`; an empty tag reconciles every ready draft |
| Rebuild next | `gh workflow run next-release.yml` (optionally `-f sha=<sha>`) |
| See whether anything is stuck | `just release-drift` |
| Publish `phux-protocol` | `gh workflow run publish-crate.yml -f tag=vX.Y.Z -f dry_run=false` |
| Revalidate an integration tag | `gh workflow run agent-integration-release.yml -f tag=<tag> -f dry_run=true` |
| Re-attach mobile FFI | `gh workflow run ffi-xcframework.yml -f tag=vX.Y.Z` |
| Report a hand-recovered release to Linear | `gh workflow run linear-release.yml -f tag=vX.Y.Z` |

## Stable

1. Conventional commits land on `main`. Release Please keeps one release PR
   for the root and one for Desktop. Each carries the version bump,
   changelog, and synced lockfiles.
2. The merged PR becomes a tag and a private draft.
3. `publish` (on `ci` or Release Please completion, daily at 15:05 UTC, or by
   dispatch) picks every draft whose tagged commit has a green `ci`. It
   deletes drafts older than a published version of the same component.
4. Each lane builds from the tag with the harness from `main`, attaches every
   asset, then flips the draft public. A CLI release must have all three
   targets, or it stays a draft.
5. `publish` rebuilds `stable.json` from what is now published, and its
   scheduled run checks for drift.

The build source is the tag, and the build harness is `main`, because
`workflow_run` and `schedule` run the default branch. So a release
infrastructure fix applies to an older tag on the next `publish` run.
`scripts/check-release-orchestration.mjs` pins this wiring.

## Next

`next-release` runs after `ci` succeeds on a push to `main`, one run at a
time; a newer commit queues. Each product rebuilds only when its inputs
changed since the commit `next.json` names. After a build lands,
`release_manifest.py next` uploads the assets, rewrites that product's entry
in `next.json`, writes its bridge record (`channel.json` or
`desktop-channel.json`), and prunes that product's assets to the current and
previous commit.

## Manifests

Clients read only `releases/download/channels/{stable,next}.json`; see
[ADR-0176](./adr/0176-the-release-pipeline.md) for the schema and the name
table. `release_manifest.py` takes digests from GitHub's asset `digest`
field, uploads only when content changes, and keeps a product's previous
entry when that product cannot be resolved. The golden manifests in
`scripts/fixtures/release-manifest/` are read by the generator, CLI, Desktop,
and installer tests. `python3 scripts/ci/release_manifest.py fixtures --check`
keeps them in step with the generator.

## Versioning

The workspace shares one `[workspace.package].version`, and every crate
inherits it. Desktop and each integration version independently under their
own Release Please packages. Do not hand-edit a version. Release Please writes
it from the commit log, and `scripts/check-release-version.sh` fails a build
whose tag disagrees with Cargo's resolved versions.

| Commit | Bump (pre-1.0) |
|---|---|
| `fix:` | patch |
| `feat:` | minor |
| `feat!:` / `BREAKING CHANGE:` | minor, not 1.0.0 (`bump-minor-pre-major`) |

Release Please cannot update `Cargo.lock`. Its `sync-lockfile` job commits the
root, browser, and Desktop lockfiles. It force-pushes the release branch on
every `main` commit, so `release-please.yml` returns the PR to draft until
the lockfile push lands. Do not merge a release PR while it is a draft.
`node scripts/check-agent-integration-versions.mjs` checks integration
versions after any version-bearing change.

## Build rules

- Release binaries use rustup and the pinned Zig tarballs from
  `.config/zig-toolchain.json`, never Nix, because Nix links `/nix/store`
  paths. Bumping Zig means re-pinning all three digests in one commit;
  `just zig-pin-check` compares them against ziglang.org.
- `scripts/build-release-binaries.sh` pins CPU baselines (`x86-64`, `generic`,
  `apple-m1`; Zig `baseline`). `scripts/check-release-cpu-baselines.sh`
  rejects a release surface that bypasses them.
- `scripts/check-binary-portability.sh` limits macOS links to the system and
  Linux links to the glibc runtime, at glibc 2.35 or lower.
- `scripts/pack-release.sh` is the only writer of the tarball layout:
  `phux-<id>-<target>/{phux, phux-mcp, README.md, LICENSE, NOTICE,
  THIRD-PARTY-NOTICES.md}`. `phux update` and the installer refuse any other
  member set, so change them together.
- Desktop is qualified before upload. The archived app must pass
  `clients/desktop/scripts/smoke-app.ts` against an isolated server built from
  the same checkout, and the installer transaction tests must pass.
- The mobile FFI workflows build the commit their run names, key concurrency
  on that commit, and fail if provenance names any other. phux-mobile pins
  `PHUX_REV` to that provenance's `phux-rev`. The build scripts
  (`scripts/build-*ffi*.sh`) define the provenance keys.

## Integrations

npm packages publish through
[trusted publishing](https://docs.npmjs.com/trusted-publishers) (OIDC), so
there is no npm secret. `publish` dispatches `agent-integration-release.yml`
instead of calling it, because npm trusts the entry workflow's filename.
Trusted publishing cannot do a package's first publish, so a new `@phux/*`
package needs one human `npm publish`. A re-dispatched tag verifies instead
of republishing.

## crates.io

Publishing is irreversible, so `publish-crate.yml` runs only by hand, against
an existing tag, with `dry_run` defaulting to `true`. It runs in the
`crates-io` environment, which requires a reviewer and holds
`CARGO_REGISTRY_TOKEN`. Dry-run locally with `just publish-protocol-dry`.
Every other crate is `publish = false`, so the CLI ships only as release
artifacts and Homebrew.

## Secrets

| Secret | Used by |
|---|---|
| Release App (`vars.RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`) | `release-please.yml`, `website-dispatch.yml`; its pushes trigger `ci`, which `GITHUB_TOKEN` pushes cannot |
| `CARGO_REGISTRY_TOKEN` | `publish-crate.yml`, in the `crates-io` environment |
| `LINEAR_RELEASE_ACCESS_KEY` | `linear-release.yml` |
