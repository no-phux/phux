---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-18
---

# Releasing

**TL;DR.** After every green `main` ci, `release.yml` runs Release Please
(versions, tags, private drafts), then ships every draft whose tagged commit
has a green `ci` and rebuilds `channels/stable.json`. `next.yml` ships green
`main` to the moving `next` prerelease and `channels/next.json`. One dispatch
of `release.yml` finishes a stuck draft. `phux-protocol` on crates.io is
published by hand.
The decisions are [ADR-0176](./adr/0176-the-release-pipeline.md). CI lanes
are in [CI](./CI.md).

## What ships

| Product | Tag | Assets | Built by |
|---|---|---|---|
| `phux` + `phux-mcp` | `vX.Y.Z` | `phux-vX.Y.Z-<target>.tar.gz` + `.sha256` + `.sha256.minisig` | `release.yml` -> `build-cli.yml` |
| Phux Desktop | `desktop-vX.Y.Z` | `phux-desktop-X.Y.Z-macos-arm64.zip`, `phux-desktop-X.Y.Z-linux-{x64,arm64}.tar.gz` + signed `.sha256`, `SHA256SUMS` | `desktop-release.yml` |
| Mobile FFI | on `vX.Y.Z` | `PhuxFFI-*`, `PhuxMobileFFI-*` zips + `.sha256` + `.provenance` | `ffi-xcframework.yml`, `ffi-android.yml` |
| `@phux/pi`, `@phux/opencode`, `@phux/omp` | `<component>-vX.Y.Z` | npm with provenance + release tarball | `agent-integration-release.yml` |
| Claude and Grok plugins | `claude-plugin-vX.Y.Z`, `grok-plugin-vX.Y.Z` | release tarball; repository marketplace | `agent-integration-release.yml` |
| `phux-protocol` | `vX.Y.Z` | crates.io | `just publish-protocol`, by hand |
| Next | `next` | `phux-next.<sha>-<target>.tar.gz`, desktop ZIP/Linux tarballs at `next.<sha>`, signed sidecars | `next.yml` |
| Manifests | `channels` | `stable.json`, `next.json` | `bun tools/main.ts release-manifest` |

CLI targets are `aarch64-apple-darwin` (`xcode-27`), `x86_64-unknown-linux-gnu`
(`ubuntu-24.04`), and `aarch64-unknown-linux-gnu` (`ubuntu-24.04-arm`). Linux
builds run in an `ubuntu:22.04` container to link against glibc 2.35. Desktop
supports Apple silicon on macOS 27 or later, ad-hoc signed and not notarized,
and native x86-64/ARM64 Linux built on Ubuntu 24.04. Linux desktop links the
host font/display libraries and requires a Vulkan driver; it does not inherit
the CLI's glibc 2.35 portability promise. Both X11 and Wayland backends are
compiled; release qualification uses X11 with Xvfb/lavapipe, not a physical
GPU or every Wayland compositor. All three desktop targets must qualify before
their checksums are signed and the draft becomes public. Only
CLI releases are GitHub's Latest. The Homebrew tap
([no-phux/homebrew-tap](https://github.com/no-phux/homebrew-tap)) renders
`Formula/phux.rb` from Latest every fifteen minutes.

## Control surface

| You want to | Do this |
|---|---|
| Ship a release | Mark the draft release PR ready, then squash-merge it once `ci` is green |
| Finish or rebuild a draft | `gh workflow run release.yml -f tag=<tag>`; an empty tag reconciles every ready draft |
| Rebuild next | `gh workflow run next.yml` (optionally `-f sha=<sha>`) |
| See whether anything is stuck | `just release-drift` |
| Publish `phux-protocol` | `just publish-protocol-dry`, then `just publish-protocol` from the tag |
| Revalidate an integration tag | `gh workflow run agent-integration-release.yml -f tag=<tag> -f dry_run=true` |
| Re-attach mobile FFI | `gh workflow run ffi-xcframework.yml -f tag=vX.Y.Z` and `ffi-android.yml` |
| Retry a failed step (Linear, a dispatch) | re-run the failed job of that `release` run |

## Stable

1. Conventional commits land on `main`. Release Please keeps one release PR
   for the root and one for Desktop. Each carries the version bump,
   changelog, and synced lockfiles. It stays a draft and keeps accumulating
   changes until a maintainer marks it ready and merges it; nothing
   auto-merges it, because `next` already ships every green `main`.
2. The merged PR becomes a tag and a private draft.
3. `release.yml` (after each green `main` ci, daily at 15:05 UTC, or by
   dispatch) picks every draft whose tagged commit has a green `ci`. It
   deletes drafts older than a published version of the same component.
4. Each lane builds from the tag with the harness from `main`, attaches every
   asset, then flips the draft public. A CLI release must have all three
   targets, or it stays a draft.
5. It rebuilds `stable.json` from what is now published, and its scheduled
   run checks for drift.
6. Once a CLI release is public, it dispatches `ffi-xcframework` and
   `ffi-android` to attach the mobile FFI, tells phux.sh to rebuild, and
   reports the release to Linear. The mobile builds run outside the release
   workflow's single concurrency slot, so the next draft never waits on them.

The build source is the tag, and the build harness is `main`, because
`workflow_run` and `schedule` run the default branch. So a release
infrastructure fix applies to an older tag on the next run.
`tools/src/cargo/release-orchestration.test.ts` pins this wiring.

## Next

`next.yml` runs after `ci` succeeds on a push to `main`, one run at a time;
a newer commit queues. Each product rebuilds only when its inputs changed
since the commit `next.json` names (`tools/src/release/next-plan.ts`). After a build lands,
`release-manifest next` uploads the assets, rewrites that product's entry
in `next.json`, writes its bridge record (`channel.json` or
`desktop-channel.json`), and prunes that product's assets to the current and
previous commit.

## Signing

Every CLI and Desktop sidecar is signed with minisign
([ADR-0178](./adr/0178-release-checksums-are-minisign-signed.md)).
`scripts/ci/sign-release.sh` reads the unencrypted secret key from the
`PHUX_MINISIGN_SECRET_KEY` repository secret, writes `<asset>.sha256.minisig`,
and fails unless the signature verifies against `RELEASE_KEYS` in
`scripts/install.sh`. It runs in `release.yml`, in the Linux `sign` job of
`desktop-release.yml` (which publishes the Desktop draft), and in `next.yml`. A missing secret fails the release instead of shipping it
unsigned. The next key's secret is held encrypted off any machine that holds
the current one; ADR-0178 decision 6 is the custody rule and the rotation
procedure.

## Manifests

Clients read only `releases/download/channels/{stable,next}.json`; see
[ADR-0176](./adr/0176-the-release-pipeline.md) for the schema and the name
table. `release-manifest` takes digests from GitHub's asset `digest`
field, uploads only when content changes, and keeps a product's previous
entry when that product cannot be resolved. The golden manifests in
`scripts/fixtures/release-manifest/` are read by the generator, CLI, Desktop,
and installer tests. `release-manifest.test.ts` keeps them in step with the
generator.

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

Release Please cannot update `Cargo.lock`, so the `release-prs` job commits
the root, browser, and Desktop lockfiles. Release Please force-pushes the
release branch on every green `main` commit, so `release.yml` returns a ready
PR to draft until the lockfile push lands, then restores it. Do not merge a
release PR while it is a draft. The tools tests check integration versions on every change.

## Build rules

- Release binaries use rustup and the pinned Zig tarballs from
  `.config/zig-toolchain.json`, never Nix, because Nix links `/nix/store`
  paths. Bumping Zig means re-pinning all three digests in one commit;
  `scripts/install-zig.sh` refuses a tarball that does not match.
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
  the same checkout, and the installer transaction tests must pass. Linux
  archives run `clients/desktop/scripts/smoke-linux.ts` under Xvfb/lavapipe,
  checking native submitted-frame evidence, independent X11 window capture,
  Tesseract OCR and shell/server
  survival across app termination. A submitted receipt is never represented
  as confirmed physical-display presentation.
- The mobile FFI workflows build the commit their run names, key concurrency
  on that commit, and fail if provenance names any other. phux-mobile pins
  `PHUX_REV` to that provenance's `phux-rev`. The build scripts
  (`scripts/build-*ffi*.sh`) define the provenance keys.

## Integrations

npm packages publish through
[trusted publishing](https://docs.npmjs.com/trusted-publishers) (OIDC), so
there is no npm secret. `release.yml` dispatches `agent-integration-release.yml`
instead of calling it, because npm trusts the entry workflow's filename.
Trusted publishing cannot do a package's first publish, so a new `@phux/*`
package needs one human `npm publish`. A re-dispatched tag verifies instead
of republishing.

## crates.io

Publishing is irreversible, so no workflow does it. Check out the tag, run
`just publish-protocol-dry`, then `just publish-protocol`, which asks for
confirmation. Every other crate is `publish = false`, so the CLI ships only as
release artifacts and Homebrew.

## Secrets

| Secret | Used by |
|---|---|
| Release App (`vars.RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`) | `release.yml` (release PRs, phux.sh), `ci.yml` (`content`: phux.sh and no-phux/skills); its pushes trigger `ci`, which `GITHUB_TOKEN` pushes cannot |
| `PHUX_MINISIGN_SECRET_KEY` | `release.yml`, `desktop-release.yml`, `next.yml` |
| `CACHIX_AUTH_TOKEN` | `ci.yml` Nix lanes push the devshell closure on `main` |
| `LINEAR_RELEASE_ACCESS_KEY` | `release.yml` |
