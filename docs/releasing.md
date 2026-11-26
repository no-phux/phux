---
group: internals
order: 8
---

# Releasing

What ships and how it is published. Each workflow under
[`.github/workflows`](../.github/workflows) documents itself at the top.

## What ships

| Product | Tag | Workflow |
| ------- | --- | -------- |
| `phux` + `phux-mcp` (macOS arm64, Linux x86_64 and arm64), SBOMs, `.deb`/`.rpm` | `vX.Y.Z` | `release.yml` → `build-cli.yml`, `packages.yml` |
| mobile FFI (xcframework, Android) | attached to `vX.Y.Z` | `ffi-xcframework.yml`, `ffi-android.yml` |
| `@phux/pi`, `@phux/opencode`, `@phux/omp` on npm; Claude and Grok plugins | `<component>-vX.Y.Z` | `agent-integration-release.yml` |
| `phux-protocol` on crates.io | `vX.Y.Z` | `just publish-protocol`, by hand |
| Phux Rune and its feed `rune.json` | `rune-vX.Y.Z` | `rune-client.yml` → `rune-build.yml` |
| `next` prerelease | moving `next` tag | `next.yml` |
| channel manifests `stable.json`, `next.json`, `lts.json` (each `.minisig`) | the `channels` release | `bun tools/main.ts release-manifest` |

Linux CLI builds link glibc 2.35 (an `ubuntu:22.04` container). Only CLI
releases are GitHub's "Latest". The Homebrew tap polls for stable releases and
renders the formula with [`packaging/gen-formula.sh`](../packaging/gen-formula.sh);
`release.yml` only nudges it with a `phux-release` dispatch.

## Stable

1. Conventional commits land on `main`. Release Please keeps one draft
   release PR for the CLI and integrations: one commit with the version
   bump, changelogs, and every lockfile entry the bump moves.
2. `phux-auto-release.yml` marks it ready two hours after the last stable
   release (dispatch skips the wait), which runs `ci`. A later run enables
   squash auto-merge only when
   `bun tools/main.ts pr-ready-ci PR_NUMBER EXPECTED_FULL_SHA` passes
   ([drafts and arming](./ci.md#drafts-and-automated-arming)). Merging creates
   the tag and a draft release. Failed checks are findings: never rerun or
   cycle draft/ready to get past them. `PHUX_AUTO_RELEASE=false` stops it.
3. `release.yml` runs on `ci` and `rune-macos` completions on `main`, daily,
   and by dispatch. A draft ships only when its exact SHA has a green `main`
   push `ci` and a full native receipt
   ([native qualification](./ci.md#native-apple-qualification)). It builds
   from the tag with the harness on `main`, so an infrastructure fix applies
   to an older tag on the next run. A CLI release needs all three targets and
   both CycloneDX SBOMs (`phux`, `phux-mcp`) or it stays a draft.
4. It signs the checksums, publishes, rebuilds `stable.json`, runs
   `packages.yml`, dispatches the mobile FFI builds and the website rebuild,
   and reports the release to Linear.

## Next

`next.yml` runs after each green `ci` on `main`, one at a time. Each product
rebuilds only when its inputs changed since the commit `next.json` names.

## LTS

`lts` gives installs fewer, vetted updates (`phux channel lts`, or the
installer's `--channel lts`). `channels/lts.json` names one stable release a
maintainer promoted; nothing moves it automatically.

- **Policy.** Promote about monthly from that month's soaked stable releases.
  The promoted `X.Y` line gets security fixes for 12 months, as patches from
  an `lts/X.Y` branch; other fixes only when severe, features never.
  [SECURITY.md](../SECURITY.md) lists the supported versions.
- **Soak.** `soak.yml` runs `just soak` nightly on the newest stable release,
  or on any tag (`gh workflow run soak.yml -f ref=<tag>`), and sets a `soak`
  commit status. A newer failing run withdraws an older pass.
- **Promote.** `gh workflow run lts.yml -f tag=vX.Y.Z` refuses a tag that is not
  a complete, signed CLI release or lacks a passing `soak` status. Clients
  refuse to move backwards without a `--version` pin, so withdraw a bad LTS by
  promoting a fixed patch.
- **Backport.** Fix `main` first. Branch `lts/X.Y` once from the
  line's newest patch tag. On a branch from it, `git cherry-pick -x`, bump the
  patch in `Cargo.toml`, `cargo update --workspace`, add the `CHANGELOG.md`
  entry, and open the PR against `lts/X.Y`. Then tag, soak, publish, promote.

Publishing a backport is not automated, and there is no manual path: `ci`
runs on `main` pushes only, `release.yml` publishes only green `main`
commits and marks them Latest (a backport must be `--latest=false`), and
the minisign key is an Actions secret. The first backport needs all three
changed.

## Linux packages

`release.yml`'s `cli` job repackages the Linux tarballs with nfpm
([`pack-linux-packages.sh`](../packaging/pack-linux-packages.sh)), signs the
`.rpm` files, and attaches them. `packages.yml` then rebuilds the apt (flat)
and dnf repositories from the three newest releases with packages
([`build-package-repo.sh`](../packaging/build-package-repo.sh)), signs them, and
replaces the `repodata` release's assets. It also pushes the AUR `phux-bin`
PKGBUILD when `AUR_SSH_PRIVATE_KEY` is set. Users add the repositories through
the [install script](./install.md#linux-package-repositories).

```sh
gh workflow run packages.yml --ref main    # republish without a release
bash packaging/test-linux-packages.sh      # rehearse in Ubuntu, Fedora and Arch containers (docker, gh)
```

Rehearse after changing any packaging script. Packages need glibc 2.35, so
RHEL 9 is refused.

**Keys.** The public key is [`packaging/phux.asc`](../packaging/phux.asc),
embedded in `install.sh` and served as `repodata/phux.asc`.
`PHUX_PACKAGES_GPG_KEY` holds only the signing subkey; the primary key stays in
the maintainer's offline backup, and `build-package-repo.sh` refuses a subkey
of any other primary. If the subkey leaks, revoke it with the primary, add a new
subkey, update `packaging/phux.asc` and the secret, and regenerate
`install.sh` (`sh install/generate.sh`). Existing users need the new
public key before newly signed packages install.

## Phux Rune

Rune skips Release Please. Every two hours `rune-auto-release.yml` opens a
draft patch bump when feat, fix or perf commits touched `clients/rune`, marks
it ready once, and arms auto-merge through the same `pr-ready-ci` gate; then
it tags the merged bump once `ci-green` passes
(`clients/rune/scripts/release.py`). The bump also puts each of those
changes on top of `clients/rune/news.json`, the notes behind the bell's
What's new panel; edit a title there in the bump PR to polish it.
`RUNE_AUTO_RELEASE=false` stops it; merge your own bump to ship hand-written
notes. Clients read only `X.Y.Z` and `X.Y.Z-alpha.N` versions.

By hand:

1. On a branch, set `version` in `clients/rune/Cargo.toml`, run
   `cargo update --workspace` in `clients/rune`, and rewrite
   `clients/rune/RELEASE_NOTES.md`, starting `**Phux Rune VERSION**`, and add
   the bell's notes to the top of `clients/rune/news.json`. Merge it.
2. Once `bun tools/main.ts ci-green <sha>` passes, push `rune-vVERSION`.
3. `rune-client.yml` refuses a tag that clients cannot read, disagrees with
   `Cargo.toml` or the notes, or fails `ci-green`; a refused tag costs its
   number, unless `ci` or `rune-macos` was still running on the commit:
   then rerun the job. `rune-build.yml` then packages, signs, notarizes, and launches the
   final archive against a same-source server (`clients/rune/scripts/qualify.sh`).
   The feed moves last; rerunning an older tag leaves it alone.

**Floors.** Linux archives need glibc 2.39 (the `ubuntu-24.04` build host).
macOS needs 14.0 (`LSMinimumSystemVersion`); qualification fails if the
binary's Mach-O floor rises above it.

### A bad Rune release

Clients refuse older versions, so the way out is forward: put the last good
feed back, say so on the bad release, and ship a higher version. The steps
are in [the bad Rune release runbook](./runbooks/bad-rune-release.md).

## Doing things by hand

```sh
gh workflow run phux-auto-release.yml      # arm the release PR now
gh workflow run release.yml -f tag=<tag>   # finish a stuck draft (no tag: all ready drafts)
just release-drift                         # list stuck drafts and releases
gh workflow run next.yml
gh workflow run agent-integration-release.yml -f tag=<tag> -f dry_run=true
gh workflow run ffi-xcframework.yml -f tag=vX.Y.Z
```

## Versioning

The workspace has one version, inherited by every crate; Phux Rune and each
integration version independently. Release Please writes versions from the
commit log (`release-please-config.json`, `.release-please-manifest.json`);
never edit one by hand. `packaging/check-release-version.sh` fails a build whose
tag disagrees with Cargo. Pre-1.0, `fix:` bumps the patch and `feat:` or a
breaking change bumps the minor.

The root bump rewrites `Cargo.toml` and the workspace crates in every
`Cargo.lock` through jsonpaths in `release-please-config.json`. After adding or
removing a workspace crate, run
`bun tools/main.ts release-orchestration-check --fix`; the tools tests fail
until you do.

## Signing and verification

`packaging/sign-release.sh` minisigns every CLI and Rune checksum and each
channel manifest with `PHUX_MINISIGN_SECRET_KEY`, and checks the signature
against the public keys compiled into `scripts/install.sh` and `phux update`.
A missing secret fails the release. On `lts` an invalid or missing manifest
signature refuses; on `stable` and `next` it warns, and the signed checksum is
the gate.

Release binaries use rustup and pinned Zig, never Nix. Only
`packaging/pack-release.sh` writes the tarball layout clients unpack.

### macOS desktop signing

Rune's macOS app and the CLI's macOS binaries are Developer ID signed with
the hardened runtime and notarized; `next`, pull-request and local builds are
ad hoc. The secrets are `APPLE_DEVELOPER_ID_P12` (base64 Developer ID
**Application** certificate with its private key), `APPLE_DEVELOPER_ID_PASSWORD`,
and a notarization-capable App Store Connect API key (`APPLE_NOTARY_KEY_P8`
base64, `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID`). Set them with
`gh secret set` from files or stdin, never command arguments.
`packaging/sign-macos-desktop.sh --check-config` checks them before a build;
missing credentials fail closed.

### Key rotation

Clients and the installer [trust a current and a next public key](../crates/phux-client-runtime/src/release/signature.rs).
Keep the next private key encrypted and off every machine that holds the
current one. To rotate, switch the signing secret to the pre-published next
key, then ship it as current with a fresh next key in both the verifier and
the installer. If current leaks, rotate to next immediately. If only next
leaks, replace it in a release signed by current.

## npm and crates.io

npm packages use trusted publishing (OIDC), so there is no npm token; a
package's first publish is done by a person. Publish `phux-protocol` from its
tag with `just publish-protocol-dry`, then the irreversible
`just publish-protocol`, using your own Cargo credentials. Every other crate is
`publish = false`.

## Secrets

Release workflows act as the release App (variable `RELEASE_APP_ID`,
secret `RELEASE_APP_PRIVATE_KEY`), never `GITHUB_TOKEN`, whose pushes trigger no
`ci`. `LINEAR_RELEASE_ACCESS_KEY` feeds the `announce` job; the signing secrets
are named in their sections above.
