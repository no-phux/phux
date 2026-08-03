---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-07-14
---

# Releasing

**TL;DR.** Releases are cut by **release-please**, not by hand. Land
conventional commits on the default branch; release-please keeps an open
"release PR" that bumps the workspace version and regenerates `CHANGELOG.md`.
Merging that PR tags `vX.Y.Z`, creates the GitHub release, and triggers
`release.yml` to build and **attach** the `phux and phux-mcp artifacts` and
refresh the Homebrew tap. Publishing `phux-protocol` to crates.io stays a
separate, deliberate human dispatch. `cargo install phux is unsupported`
because the binary/internal crates are not publishable. Windows is not
supported by this release lane.

## Who owns what

This boundary is load-bearing; blurring it makes two workflows fight over the
same release.

| Thing | Owner |
|---|---|
| Version bump in `Cargo.toml`, `CHANGELOG.md` | release-please (via the release PR) |
| `Cargo.lock` refresh on the release PR | the `sync-lockfile` job in `release-please.yml` |
| The `vX.Y.Z` **tag** | release-please, when the release PR merges |
| The GitHub **release** and its body/notes | release-please |
| Release **assets** (tarballs + `.sha256`) | `release.yml`, via `gh release upload` |
| Homebrew tap formula | `release.yml` |
| `phux-protocol` on crates.io | a human, via `publish-crate.yml` |

`release.yml` never creates a tag and never creates or edits a release. It
uploads assets onto the release release-please already made, so it cannot
clobber the generated changelog.

## Release cockpit

| You want to | Do this |
|---|---|
| Ship a release | Mark the open **release-please** PR "Ready for review" (it is born draft; undrafting runs CI), then merge it |
| Prove the release is locally coherent first | `just release-preflight vX.Y.Z` |
| Skip crates.io packaging during a fast/offline binary-only check | `just release-preflight-fast vX.Y.Z` |
| Re-build or re-attach assets for an existing tag | Dispatch **Actions -> release** with `tag=vX.Y.Z` |
| Publish `phux-protocol` to crates.io | Dispatch **Actions -> publish-crate** with `tag=vX.Y.Z`, `dry_run=false` |
| Check a suspected install-doc drift | `bash scripts/check-install-surface.sh` |

## What runs when

| Flow | Trigger | What it does |
|---|---|---|
| Pull request CI | `pull_request` | Docs-only detection, docs check, fmt, clippy, rustdoc, cargo-deny, unit tests, and fast real-PTY e2e unless the change is docs-only. Draft PRs skip all of it until marked ready — release PRs idle as drafts for free. |
| Conventional-commit gate | `pull_request` | `commitlint` lints every PR commit and the PR title against `commitlint.config.mjs`; required by main's ruleset so nothing non-conventional reaches the release-please log. |
| Main CI | push to `main` | Same gates as PR CI, always full, and refreshes the warm caches. |
| release-please | push to `main` | Maintains the release PR; on merge, tags `vX.Y.Z`, creates the GitHub release, and calls `release.yml`. |
| Release artifacts | called by release-please (or manual dispatch) | Builds portable tarballs + checksums, attaches them to the existing release, updates the Homebrew tap. |
| Crate publish | manual `publish-crate` workflow | `phux-protocol` package dry-run, then publish when `dry_run=false`. |
| Stress lane | nightly, manual, or PR label `stress` | Heavy resize/output/lifecycle storms that are useful but too slow for every PR. |
| Build timings | manual | Cold `cargo --timings` report for compile-time diagnosis. |

Required secrets:

| Secret | Used by | Required for |
|---|---|---|
| `HOMEBREW_TAP_DEPLOY_KEY` | `release.yml` | Automatic push to `phall1/homebrew-tap`. If absent, the release still publishes and the tap step warns/skips. |
| `CARGO_REGISTRY_TOKEN` | `publish-crate.yml` | Publishing `phux-protocol` to crates.io. Not needed for binary/Homebrew-only releases. |

Post-release verification:

```sh
scripts/install.sh --dry-run --version vX.Y.Z
brew fetch --formula phall1/tap/phux
cargo search phux-protocol --limit 1
```

Use the GitHub release page to confirm that the expected target tarballs and
`.sha256` sidecars uploaded. The current release lane builds macOS arm64,
Linux x86_64, and Linux arm64.

## What ships where

| Artifact | Channel | Mechanism |
|---|---|---|
| `phux`, `phux-mcp` binaries | Homebrew + GitHub release | [`release.yml`](../.github/workflows/release.yml), called by release-please |
| `phux-protocol` crate | crates.io | [`publish-crate.yml`](../.github/workflows/publish-crate.yml), manual dispatch only |

Every other crate (`phux`, `phux-core`, `phux-server`, `phux-client`,
`phux-config`, `phux-mcp`) is `publish = false`: binary or internal-only.
The installable CLI ships through release artifacts and Homebrew instead of
`cargo install phux`.

Each binary release must produce `phux and phux-mcp artifacts` for every target
that publishes. The tarball layout is:

```text
phux-<tag>-<target>/
  phux
  phux-mcp
  README.md
  LICENSE-MIT
  LICENSE-APACHE
```

The workflow smoke-checks both binaries in the staging directory before it
creates `phux-<tag>-<target>.tar.gz` and the matching `.sha256` sidecar.
Homebrew installs both binaries from the same tarball.

## Versioning

The workspace shares one `version` in the root `Cargo.toml`
(`[workspace.package]`). All in-repo crates inherit it with
`version.workspace = true`, and internal workspace dependencies use path-only
requirements so release bumps do not require duplicate manifest edits.

**Do not hand-edit the version.** release-please derives it from the
conventional-commit log and writes it into `[workspace.package].version` on the
release PR (via a TOML jsonpath updater configured in
`release-please-config.json`). The `sync-lockfile` job then runs
`cargo update --workspace` on the same PR so `Cargo.lock` matches; release-please
cannot update a lockfile itself.

Pre-1.0 bump rules, set in `release-please-config.json`:

| Commit | Bump |
|---|---|
| `fix:` | patch (0.1.0 -> 0.1.1) |
| `feat:` | minor (0.1.0 -> 0.2.0) |
| `feat!:` / `BREAKING CHANGE:` | minor (0.1.0 -> 0.2.0), **not** 1.0.0 |

`bump-minor-pre-major: true` is what keeps a breaking change from catapulting
the project to 1.0.0. Do not remove it without meaning to.

A safety net backs the whole scheme: `scripts/check-release-version.sh` runs in
`release.yml` at the tag and fails the release if the tag does not match Cargo's
resolved package versions. That is the gate that catches a silently-no-op'd
version updater, so do not remove it.

## Cutting a full release

1. Land conventional commits on the default branch.
2. Review the open **release-please** PR: it bumps `[workspace.package].version`,
   regenerates `CHANGELOG.md`, and carries a synced `Cargo.lock`.
3. Optionally verify locally: `just release-preflight vX.Y.Z` for the version it
   proposes.
4. Merge the release PR.

release-please then tags `vX.Y.Z`, creates the GitHub release with the generated
changelog as its body, and calls `release.yml`, which validates the tag against
Cargo's resolved versions, builds `phux` + `phux-mcp` for
`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, and
`aarch64-unknown-linux-gnu`, packages `phux-<tag>-<target>.tar.gz` + `.sha256`,
uploads them onto that release, and — if the `HOMEBREW_TAP_DEPLOY_KEY` secret is
set — regenerates and pushes `Formula/phux.rb` to the tap.

Release builds use rustup plus the official Zig tarballs instead of the Nix dev
shell, because portable release binaries must not record `/nix/store` dynamic
library paths. The workflow checks macOS binaries with `otool -L` before
packaging.

Those tarballs are pinned by SHA-256 in `release.yml`, one digest per target,
and the digests are hand-written on purpose — a checksum fetched at build time
would verify nothing about the server that served the tarball. **Bumping
`ZIG_VERSION` means re-pinning all three digests in the same commit.** Missing
that is what published `v0.10.0` with no assets: the version moved to `0.16.0`
while every digest stayed on `0.15.2`, so all three matrix legs failed at
`shasum -c`, and release.yml runs only after the tag and release already exist.
`just zig-pin-check` (`scripts/check-zig-pins.sh`, a `just ci` and ci.yml step)
compares the pins against `https://ziglang.org/download/index.json` and fails on
a stale one; it skips itself when the index is unreachable. `v0.0.3` is the current portable public release. `v0.0.1` was seeded
with a Linux x86_64 tarball plus checksum, but that first artifact is Nix-linked
and not portable; do not point installers or the tap at it.

For an emergency host-only artifact, use the same dist layout locally:

```sh
cargo build --release --bin phux --bin phux-mcp
just dist vX.Y.Z                       # -> dist/phux-vX.Y.Z-<host>.tar.gz (+ .sha256)
gh release upload vX.Y.Z dist/*        # attach the tarball + checksum
```

Do not use this for normal releases. Do not run a local release build inside
`nix develop`; use a host toolchain plus Zig on `PATH` so the packaged binaries
do not link to Nix-store libraries.

### Required secret

`HOMEBREW_TAP_DEPLOY_KEY` — the **private** half of an SSH key whose
public half is a write-enabled deploy key on `phall1/homebrew-tap`.
Without it the release still publishes; only the automatic formula bump
is skipped (a warning annotation is emitted). The formula itself is
produced by [`scripts/gen-formula.sh`](../scripts/gen-formula.sh), which
emits a stable top-level URL plus overrides only for the targets that actually
built — so a partial-matrix release still yields an installable formula.

Because a platform with no matching `on_*` override silently falls back to that
top-level URL, the generator also emits a fatal `depends_on` guard for every
platform with no artifact. macOS ships arm64 only, so the formula carries
`depends_on arch: :arm64` inside `on_macos`: an Intel Mac is refused at install
time instead of receiving an arm64 binary that cannot exec.

### Curl installer contract

The curl installer is a convenience layer over GitHub release artifacts. The
unversioned command is user-facing because `v0.0.3` or newer is now the latest
GitHub release:

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash
```

Keep it aligned with the release layout above. It should download the target
tarball and `.sha256` sidecar from the selected release, verify the checksum
before unpacking, and install `phux` + `phux-mcp` into
`${PHUX_INSTALL_DIR:-$HOME/.local/bin}`. With no `--version`, it resolves the
current GitHub release. Keep the explicit `v0.0.1` refusal as a historical
safety guard. User-facing docs should identify `v0.0.3` or newer as the current
portable release.

### CPU baseline caveat

`libghostty-vt`'s `build.rs` lets zig auto-detect the host CPU for
native builds, so Linux artifacts may carry instructions specific to the
runner generation and can `SIGILL` on older hardware. `aarch64-apple-darwin`
has a uniform baseline and is unaffected. Pinning Linux CPU baselines through
`libghostty-vt`'s build is future work.

## Publishing phux-protocol to crates.io

Publishing is irreversible — versions cannot be reused and the name cannot be
reclaimed. It is therefore **not** wired into the release-please path: a
tag-triggered workflow has no human to confirm anything, so `release.yml` does
not publish at all. `publish-crate.yml` is the only path, dispatched by hand
against an existing tag, with `dry_run` defaulting to `true`.

1. Settle `docs/spec/` + the `phux-protocol` version (see
   [`CONTRIBUTING.md`](../CONTRIBUTING.md)).
2. Dry-run locally: `just publish-protocol-dry` (packages + verifies;
   the default feature set has no git deps, so it builds clean).
3. Authenticate: `cargo login` once on the publishing machine, or set
   the `CARGO_REGISTRY_TOKEN` secret for the workflow.
4. Publish: dispatch `publish-crate.yml` with `tag: vX.Y.Z` and
   `dry_run: false`, or run `just publish-protocol` locally.

The publish job runs in the `crates-io` GitHub Environment. Configure that
environment with a required reviewer and scope `CARGO_REGISTRY_TOKEN` to it, so
the irreversible step needs a second pair of eyes.

The `server` feature's optional `libghostty-vt` resolves to the
crates.io release (`>= 0.2.0`) for external consumers; verify that
release is API-compatible with the workspace dependency before relying on
the `server` feature downstream.

Do not publish the binary crate or internal workspace crates as part of
this workflow. For users, the idiomatic crates.io command is
`cargo add phux-protocol`; `cargo install phux is unsupported` until
the binary crate and its internal dependencies are intentionally made
publishable.

## Installing from the tap

```sh
brew install phall1/tap/phux
```

The tap does not add Windows support; Windows is not supported here. A Windows
release would need a separate design and build lane rather than a formula tweak.
