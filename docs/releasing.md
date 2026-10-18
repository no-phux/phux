# Releasing

`release.yml` and `next.yml` do the releasing. This page says what the pieces are and which
buttons exist. The workflows under
[`.github/workflows`](../.github/workflows) are the reference; each one
explains itself at the top.

## What ships

| Product                                                      | Tag                                          | From                                     |
| ------------------------------------------------------------ | -------------------------------------------- | ---------------------------------------- |
| `phux` + `phux-mcp` (macOS arm64, Linux x86_64, Linux arm64) | `vX.Y.Z`                                     | `release.yml` → `build-cli.yml`          |
| Phux Desktop (macOS arm64, Linux x64 and arm64)              | `desktop-vX.Y.Z`                             | `desktop-release.yml`                    |
| mobile FFI (xcframework, Android)                            | attached to `vX.Y.Z`                         | `ffi-xcframework.yml`, `ffi-android.yml` |
| `@phux/pi`, `@phux/opencode`, `@phux/omp` on npm             | `<component>-vX.Y.Z`                         | `agent-integration-release.yml`          |
| Claude and Grok plugins                                      | `claude-plugin-vX.Y.Z`, `grok-plugin-vX.Y.Z` | same; GitHub release only                |
| `phux-protocol` on crates.io                                 | `vX.Y.Z`                                     | `just publish-protocol`, by hand         |
| `next` prerelease                                            | moving `next` tag                            | `next.yml`                               |
| channel manifests `stable.json`, `next.json`                 | the `channels` release                       | `bun tools/main.ts release-manifest`     |

Linux CLI builds link against glibc 2.35 (built in an `ubuntu:22.04`
container). The desktop does not inherit that promise. macOS desktop builds
are ad-hoc signed and not notarized. Only CLI releases are GitHub's
"Latest"; the Homebrew tap renders its formula from Latest on a schedule.

## Stable

1. Conventional commits land on `main`. Release Please keeps one draft
   release PR for the root and one for the desktop, each carrying the
   version bump, changelog, and synced lockfiles. Nothing merges it
   automatically.
2. A maintainer marks the release PR ready and squash-merges it once `ci`
   is green. That creates the tag and a private draft release.
3. `release.yml` runs after a successful push `ci` on `main`, daily,
   or by dispatch. It picks every draft whose exact tagged commit has a successful push
   `ci` on `main` (not another commit's green status), builds the assets from the tag using the harness on `main`,
   attaches them, signs the checksums, and flips the draft public. A CLI
   release needs all three targets or it stays a draft.
4. It then rebuilds `stable.json`, dispatches the mobile FFI builds, and
   dispatches the website repository to rebuild phux.sh, and reports the
   CLI release to Linear from the `announce` job.

Because the harness comes from `main`, a release-infrastructure fix applies
to an older tag on the next `release` run.

## Next

`next.yml` runs after each green `ci` on `main`, one at a time.
Each product rebuilds only when its inputs changed since the commit
`next.json` names. Assets are kept for the current and previous commit.

## Doing things by hand

| You want to                     | Run                                                                          |
| ------------------------------- | ---------------------------------------------------------------------------- |
| finish or rebuild a stuck draft | `gh workflow run release.yml -f tag=<tag>` (empty tag: every ready draft)    |
| rebuild next                    | `gh workflow run next.yml`                                                   |
| see what is stuck               | `just release-drift`                                                         |
| publish `phux-protocol`         | `just publish-protocol-dry`, then `just publish-protocol` from the tag       |
| revalidate an integration tag   | `gh workflow run agent-integration-release.yml -f tag=<tag> -f dry_run=true` |
| re-attach mobile FFI            | `gh workflow run ffi-xcframework.yml -f tag=vX.Y.Z`                          |

## Versioning

The workspace has one version, inherited by every crate; the desktop and
each integration version independently. Release Please writes versions from
the commit log, and `scripts/check-release-version.sh` fails a build whose
tag disagrees with Cargo. Do not edit a version by hand.

Pre-1.0: `fix:` bumps the patch, `feat:` bumps the minor, and a breaking
change also bumps the minor.

Release Please cannot update `Cargo.lock`; the `release-prs` job commits
the lockfiles to the release branch, which returns the PR to draft until
that push lands. Do not merge a release PR while it is a draft.

## Signing and verification

Every CLI and desktop checksum is signed with minisign by
`scripts/ci/sign-release.sh`, using the `PHUX_MINISIGN_SECRET_KEY` secret,
and the signature is checked against the public keys compiled into
`scripts/install.sh` and `phux update` before anything is attached. A
missing secret fails the release rather than shipping unsigned.

Release binaries use rustup and the pinned Zig tarballs, never Nix, so no
`/nix/store` path leaks into them. `scripts/build-release-binaries.sh`
pins CPU baselines; `scripts/check-binary-portability.sh` limits what the
binaries may link; `scripts/pack-release.sh` is the only writer of the
tarball layout, which `phux update` and the installer both refuse to
unpack if it changes.

The desktop is qualified before upload: `smoke-app.ts` on macOS, and
`smoke-linux.ts` under Xvfb and lavapipe on Linux, against an isolated
server built from the same checkout.

## Release-key custody and rotation

The verifier [trusts current and next public keys](../crates/phux-client-runtime/src/release/signature.rs);
the installer embeds the same list and tests pin parity. Keep the **next
private key encrypted and off machines holding the current private key**,
including the build workstation. Never put private key material in docs,
logs, source or review artifacts. Separating custody prevents one compromise
from stealing both the signer and its pre-trusted replacement.

For an authorized rotation:

1. Switch the Actions signing secret to the already pre-published next key.
   Existing clients trust it; do not generate an untrusted signer first.
2. Ship that key as current plus a fresh next public key in both the Rust
   verifier and installer, with parity/signature tests. Secure the fresh
   next private key separately before relying on it.
3. If current leaks, rotate to the pre-trusted next immediately. If only
   next leaks, replace next through a release signed by the safe current.

Clients too old to trust either remaining key need an externally trusted
reinstall after missed rotations. Signed checksum sidecars authenticate
artifacts, not freshness: the channel manifest is unsigned, and a next-channel
rollback to an older validly signed build remains possible. These are trust
limitations, not permission to skip signature verification.

## npm and crates.io

npm packages publish with trusted publishing (OIDC), so there is no npm
token; a package's first publish has to be done by a person. `phux-protocol`
is published manually from its verified tag: run `just publish-protocol-dry`,
then the confirmation-gated, irreversible `just publish-protocol`. The
operator supplies Cargo authentication (`cargo login` or
`CARGO_REGISTRY_TOKEN`); no crates.io publishing workflow remains. Every
other crate is `publish = false`.

## Secrets

| Secret                                                    | Used by                                                                                                |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| Release App (`RELEASE_APP_ID`, `RELEASE_APP_PRIVATE_KEY`) | `release.yml`: release PRs and website dispatch; App pushes trigger `ci`, unlike `GITHUB_TOKEN` pushes |
| `PHUX_MINISIGN_SECRET_KEY`                                | signing in the release workflows                                                                       |
| `CARGO_REGISTRY_TOKEN`                                    | manual operator Cargo publication, not a workflow                                                      |
| `LINEAR_RELEASE_ACCESS_KEY`                               | `release.yml` announce job                                                                             |
