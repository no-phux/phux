# Releasing

`release.yml` and `next.yml` do the releasing; `lts.yml` promotes. This page says what the pieces are and which
buttons exist. The workflows under
[`.github/workflows`](../.github/workflows) are the reference; each one
explains itself at the top.

## What ships

| Product                                                      | Tag                                          | From                                     |
| ------------------------------------------------------------ | -------------------------------------------- | ---------------------------------------- |
| `phux` + `phux-mcp` (macOS arm64, Linux x86_64, Linux arm64) | `vX.Y.Z`                                     | `release.yml` → `build-cli.yml`          |
| mobile FFI (xcframework, Android)                            | attached to `vX.Y.Z`                         | `ffi-xcframework.yml`, `ffi-android.yml` |
| `@phux/pi`, `@phux/opencode`, `@phux/omp` on npm             | `<component>-vX.Y.Z`                         | `agent-integration-release.yml`          |
| Claude and Grok plugins                                      | `claude-plugin-vX.Y.Z`, `grok-plugin-vX.Y.Z` | same; GitHub release only                |
| `phux-protocol` on crates.io                                 | `vX.Y.Z`                                     | `just publish-protocol`, by hand         |
| Phux Rune (macOS arm64, Linux x86_64, Linux arm64) and its feed `rune.json` in `channels` | `rune-vX.Y.Z-alpha.N`, `rune-vX.Y.Z` | `rune-client.yml` → `rune-build.yml` |
| `next` prerelease                                            | moving `next` tag                            | `next.yml`                               |
| CycloneDX SBOMs `phux-vX.Y.Z.cdx.json`, `phux-mcp-vX.Y.Z.cdx.json` | attached to `vX.Y.Z`                    | `release.yml` (`sbom` job)               |
| channel manifests `stable.json`, `next.json`, `lts.json`, each with a `.minisig` | the `channels` release   | `bun tools/main.ts release-manifest`     |

Linux CLI builds link against glibc 2.35 (built in an `ubuntu:22.04`
container). Phux Rune does not inherit that promise ([Floors](#phux-rune)).
Only CLI releases are GitHub's "Latest". The Homebrew tap discovers stable
`vX.Y.Z` CLI releases every fifteen minutes, verifies artifact checksums,
and renders the formula. Upstream tap credentials or dispatches are not
required. The CLI formula body comes from
[`scripts/gen-formula.sh`](../scripts/gen-formula.sh); the tap copies that
script and re-renders. The formula license is Apache-2.0, matching `LICENSE`
and the workspace `Cargo.toml`. `release.yml` does not push the tap.

The GPUI desktop (`desktop-vX.Y.Z`, last 0.62.0) is retired and no longer
released; its source is the `desktop-v0.62.0` tag.

## Stable

1. Conventional commits land on `main`. Release Please keeps one draft
   release PR for the root (CLI and integrations). It is one commit carrying the version bump, changelog, and every
   lockfile entry the bump moves.
2. Every two hours `phux-auto-release.yml` marks the release PR ready, which
   runs `ci`, and turns on squash auto-merge, so it merges once `ci` is green.
   That creates the tag and a private draft release. A maintainer can still
   merge it sooner by hand; `PHUX_AUTO_RELEASE=false` stops the automation.
3. `release.yml` runs after a successful push `ci` on `main`, daily,
   or by dispatch. It picks every draft whose exact tagged commit has a successful push
   `ci` on `main` (not another commit's green status), builds the assets from the tag using the harness on `main`,
   attaches them, signs the checksums, and flips the draft public. A CLI
   release needs all three targets or it stays a draft.
4. It then rebuilds and signs `stable.json`, dispatches the mobile FFI builds, and
   dispatches the website repository to rebuild phux.sh, and reports the
   CLI release to Linear from the `announce` job.

Because the harness comes from `main`, a release-infrastructure fix applies
to an older tag on the next `release` run.

Each CLI release also carries a CycloneDX 1.5 SBOM for `phux` and one for
`phux-mcp`, generated from the tagged workspace for every target
(`--target all`) by `cargo-cyclonedx` 0.5.10, pinned in `release.yml`. A
release without both stays a draft. They are not minisign-signed; GitHub
records each asset's SHA-256.

## LTS

`lts` is the line for installs that want fewer, vetted updates:
`curl -fsSL https://phux.sh/install | sh -s -- --channel lts`, or
`phux channel lts`. `channels/lts.json` names one stable release a
maintainer promoted; no release moves it on its own.

**Policy.**

- A release is promoted about monthly, from the stable releases of that
  month that passed the soak gate.
- The promoted minor line `X.Y` gets security fixes for 12 months after its
  promotion, as patch releases cut from an `lts/X.Y` branch. Other bug fixes
  are backported only when they are severe; features never are.
- [SECURITY.md](../SECURITY.md) lists what is supported: the latest stable and
  the current LTS.

**Soak gate.** `soak.yml` runs nightly on the newest stable release, and by
dispatch on any tag (`gh workflow run soak.yml -f ref=vX.Y.Z`). It runs
`just soak` on the tagged tree: the e2e lane, then ten rounds of the guards
for the two incidents a long-lived server hit (every graceful-upgrade test,
including the descriptor-leak check for the fd leak across upgrades, #1956;
and the per-pane systemd scope that keeps oomd off the server, #2006, run
against a real user manager), then the stress storms. The result is a
`soak` commit status on the tag's commit; a newer failing run withdraws an
older pass.

**Promotion.** `gh workflow run lts.yml -f tag=vX.Y.Z`. It refuses a tag
that is not a published CLI release with every target's tarball, checksum
and signature, or whose commit has no passing `soak` status, then publishes
`lts.json` and its signature. Clients refuse to move backwards without a
`--version` pin, so promote forward; to withdraw a bad LTS, promote a fixed
patch.

**Backports.** For a security fix to a supported line:

1. Land the fix on `main` first, as usual, unless the code is gone there.
2. Create the branch once: `git switch -c lts/X.Y vX.Y.Z` (the newest patch
   of that line), and push it. Later fixes go on the same branch.
3. On a branch from `lts/X.Y`, `git cherry-pick -x <commit>`, bump the patch in
   `Cargo.toml`, run `cargo update --workspace` so every `Cargo.lock` records
   it, and add the `CHANGELOG.md` entry. Open the pull request against
   `lts/X.Y`.
4. Once it is merged and green, tag `vX.Y.(Z+1)` on that commit, soak it,
   publish it, and promote it with `lts.yml`.

The publish step is not automated yet: `ci` runs on pushes to `main` only,
`release.yml` publishes only commits with a green push `ci` on `main`, and it
marks what it publishes GitHub's Latest, which a backport must not take
(`--latest=false`). Shipping the first backport needs those three changes
first.

## Next

`next.yml` runs after each green `ci` on `main`, one at a time.
Each product rebuilds only when its inputs changed since the commit
`next.json` names. Assets are kept for the current and previous commit.

## Phux Rune

Rune releases never go through Release Please. `rune-auto-release.yml` cuts
them on its own: every two hours it opens and auto-merges the next patch bump
when feat/fix/perf commits touched `clients/rune` since the last tag, with notes
generated from those subjects, and tags the merged bump after its push `ci`
goes green (`clients/rune/scripts/release.py`). Set the repository variable
`RUNE_AUTO_RELEASE=false` to stop it. To ship hand-written notes, merge your
own bump first: the train then only tags it. Versions ran `0.1.0-alpha.N` to
alpha.23, then `0.1.0`; the train now bumps the patch (`0.1.1`, `0.1.2`, ...).
Clients read only `X.Y.Z` and `X.Y.Z-alpha.N`, so a beta in the feed would
strand every one of them on a failed check, and a prerelease after `0.1.0`
would sort below it.

By hand:

1. On a branch, set `version` in `clients/rune/Cargo.toml`, run
   `cargo update --workspace` in `clients/rune` so `Cargo.lock` records it
   (every Rune build is `--locked`), and rewrite
   `clients/rune/RELEASE_NOTES.md`, whose first words must be
   `**Phux Rune VERSION**`. Merge it.
2. Wait for `ci` on that exact `main` commit, then push `rune-vVERSION`.
3. `rune-client.yml` refuses the tag unless clients can read it, it matches
   `Cargo.toml` and the notes, and that commit has a green push `ci` on `main`.
   If `ci` was still running, rerun the failed job. Tags can't be deleted or
   moved, so a tag refused for its version or notes costs that number: fix the
   tree and take the next one.
4. `rune-build.yml` tests, packages, signs and notarizes, then launches the
   final archive against a same-source server on a temp socket
   (`clients/rune/scripts/qualify.sh`). That checks the licenses in the
   archive, what the binary links, its version, and that a frame shows the
   session; on Linux it also opens a window under Xvfb and lavapipe. Pull
   requests run the same build as the `rune` lane of `ci`, unsigned.
5. The release goes up with `--latest=false`, a prerelease only if the version
   is one, and the feed moves last. Rerunning an older tag leaves the feed alone.

Publishing and moving the feed are one job; there is no manual promotion step.

**Floors.** Linux archives need glibc 2.39, what the `ubuntu-24.04` build host
links; this is a higher floor than the CLI's 2.35. macOS 14.0 is
`LSMinimumSystemVersion`, and qualification fails if the binary's Mach-O floor
rises above it. No deployment target is set, so that floor is rustc's arm64
default, 11.0: the plist key is what holds macOS at 14.0.

### A bad Rune release

Clients refuse older versions, so nothing published can pull a bad build back
off a machine that took it. **The way out is forward.** Two checks stand in
front of that: qualification before upload, and from alpha.8 a launch check in
the updater, which has the new binary draw one headless frame on the user's GPU
before anything is swapped. Clients on alpha.7 and earlier have no such check:
Linux runs `--version`, which exits before any GPU work, and macOS runs nothing
from the new build. Neither check opens a real window on the user's machine, so
a crash there still ships.

1. **Stop it spreading.** Put the last good feed back, so clients that haven't
   taken the bad version stop being offered it. Do this first.

   ```sh
   good=0.1.N
   dir="$(mktemp -d)"
   gh release download "rune-v$good" --repo no-phux/phux --dir "$dir" \
     --pattern '*.zip' --pattern '*.tar.gz' --pattern '*.sha256'
   git show "rune-v$good:crates/phux-protocol/src/lib.rs" > "$dir/lib.rs"
   python3 clients/rune/scripts/feed.py build "$good" \
     "$(git rev-parse "rune-v$good^{commit}")" "$dir" "$dir/lib.rs" > "$dir/rune.json"
   gh release upload channels "$dir/rune.json" --clobber --repo no-phux/phux
   ```

2. **Say so.** Edit the bad release's notes to name the problem. Keep the tag
   and the release: versions are never reused, and the next tag must sort above it.
3. **Fix forward.** Ship the fix as a higher version through the normal steps.
   The feed moves to it by itself, and clients on the bad build update as usual,
   if they still start.
4. **A build that won't start can't update itself.** Those users reinstall:
   `curl -fsSL https://phux.sh/install | sh -s -- --rune` takes whatever the feed
   names, even an older version, and keeps their settings and sessions.

The first real update from each older client on macOS and Linux is the release
evidence nothing here can replace: record alpha.6 to the next version, with
Restart now, quit-install and the server handoff, before calling it good.

## Doing things by hand

| You want to                     | Run                                                                          |
| ------------------------------- | ---------------------------------------------------------------------------- |
| finish or rebuild a stuck draft | `gh workflow run release.yml -f tag=<tag>` (empty tag: every ready draft)    |
| soak a release candidate        | `gh workflow run soak.yml -f ref=<tag>`                                      |
| promote a release to lts        | `gh workflow run lts.yml -f tag=<tag>` (needs a passing soak)                |
| rebuild next                    | `gh workflow run next.yml`                                                   |
| see what is stuck               | `just release-drift`                                                         |
| publish `phux-protocol`         | `just publish-protocol-dry`, then `just publish-protocol` from the tag       |
| revalidate an integration tag   | `gh workflow run agent-integration-release.yml -f tag=<tag> -f dry_run=true` |
| re-attach mobile FFI            | `gh workflow run ffi-xcframework.yml -f tag=vX.Y.Z`                          |

## Versioning

The workspace has one version, inherited by every crate; Phux Rune and
each integration version independently. Release Please writes versions from
the commit log, and `scripts/check-release-version.sh` fails a build whose
tag disagrees with Cargo. Do not edit a version by hand.

Pre-1.0: `fix:` bumps the patch, `feat:` bumps the minor, and a breaking
change also bumps the minor.

Release Please reads `release-please-config.json` with
`.release-please-manifest.json`, for the root and integrations.

The root train rewrites `Cargo.toml` and the workspace crates' entries in
every `Cargo.lock` in the same commit, through `toml` extra-files whose
jsonpaths name those crates. A release PR is therefore complete the moment
it exists; nothing patches it afterwards. Adding or removing a workspace
crate changes those jsonpaths: `bun tools/main.ts release-orchestration-check --fix`
regenerates them, and the tools tests fail until it has.

## Signing and verification

Every CLI and Phux Rune checksum, and each channel manifest
(`<channel>.json.minisig`), is signed with minisign by
`scripts/ci/sign-release.sh`, using the `PHUX_MINISIGN_SECRET_KEY` secret,
and the signature is checked against the public keys compiled into
`scripts/install.sh` and `phux update` before anything is attached. A
missing secret fails the release rather than shipping unsigned.

`phux update` and the installer (with `minisign` on `PATH`) check the
manifest signature when they read a channel. On `lts` a missing or invalid
signature refuses. On `stable` and `next` it only warns, because their
manifests were published unsigned before signing began; the signed artifact
checksum stays the gate there.

Release binaries use rustup and the pinned Zig tarballs, never Nix, so no
`/nix/store` path leaks into them. `scripts/build-release-binaries.sh`
pins CPU baselines; `scripts/check-binary-portability.sh` limits what the
binaries may link; `scripts/pack-release.sh` is the only writer of the
tarball layout, which `phux update` and the installer both refuse to
unpack if it changes.

### macOS desktop signing

Phux Rune's macOS releases ship a ZIP of `Phux Rune.app`, Developer ID signed
with the hardened runtime, Apple-notarized and stapled. The ZIP gets a
SHA-256 sidecar and a minisign signature like every other asset. Linux
archives are not Apple-signed. Pull-request and local builds are ad-hoc signed.

The standalone CLI's macOS archive carries `phux` and `phux-mcp` signed with
the same Developer ID (hardened runtime) and notarized, so the macOS
firewall's signed-software allowance covers them without a per-version
allowlist entry. A bare executable cannot hold a stapled ticket; Gatekeeper
checks it online. `next` builds stay ad hoc.

The Apple credentials are separate from minisign. Configure these repository
Actions secrets before marking a release PR ready (the CLI) or pushing a
`rune-v*` tag:

| Secret | Value |
| --- | --- |
| `APPLE_DEVELOPER_ID_P12` | Base64 of an exported **Developer ID Application** certificate **and private key** (`.p12`) |
| `APPLE_DEVELOPER_ID_PASSWORD` | Password protecting that P12 export |
| `APPLE_NOTARY_KEY_P8` | Base64 of an App Store Connect team API private key (`.p8`) with notarization access |
| `APPLE_NOTARY_KEY_ID` | Its 10-character key ID |
| `APPLE_NOTARY_ISSUER_ID` | Its issuer UUID |

An **Apple Distribution** or **Apple Development** certificate is not a
Developer ID Application certificate and cannot substitute for it. The Apple
Developer Program **Account Holder** creates the exportable Developer ID
certificate in [Certificates, Identifiers & Profiles](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/)
and exports it with its private key from Keychain Access. Use an authorized
team API key from App Store Connect for notarization; an existing mobile
release key may suffice if it has the required access. Keep the originals
and export password outside the repo.
Upload locally with `gh secret set --repo no-phux/phux`, reading values
from files/stdin rather than putting private material in command arguments
or chat. Do not commit P12/P8 files or notarization credentials.

`scripts/ci/sign-macos-desktop.sh --check-config` checks required settings
before an expensive build. Missing credentials fail closed: a release never
silently falls back to ad-hoc signing. The script imports the P12 into a
temporary keychain, signs with hardened runtime and a secure timestamp, waits
for Apple's `Accepted` result, staples and validates the app, and writes the
ZIP. Temporary keys and keychain are removed even on failure. `rune-build.yml`
then checks the stapled ticket and Gatekeeper's acceptance of the unpacked
download before checksumming the final bytes.

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
reinstall after missed rotations. Signatures authenticate artifacts and
manifests, not freshness: an old, validly signed manifest can be replayed,
and a next-channel rollback to an older validly signed build remains
possible (stable and lts refuse an unpinned downgrade). These are trust
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
| `PHUX_MINISIGN_SECRET_KEY`                                | signing artifacts and channel manifests in `release.yml`, `next.yml`, `lts.yml`, and `rune-client.yml` |
| `CARGO_REGISTRY_TOKEN`                                    | manual operator Cargo publication, not a workflow                                                      |
| `LINEAR_RELEASE_ACCESS_KEY`                               | `release.yml` announce job                                                                             |
