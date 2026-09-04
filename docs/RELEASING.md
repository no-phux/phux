---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-08-15
---

# Releasing

**TL;DR.** Release Please owns independent root, Cockpit, and host-integration
versions in one manifest. Merging its reviewed PR creates component tags and
private draft releases; dedicated workflows validate exact tagged trees,
attach artifacts, publish only complete releases, and update Homebrew. The
`phux-protocol` crate remains a separate human dispatch.

## Who owns what

This boundary is load-bearing; blurring it makes two workflows fight over the
same release.

| Thing | Owner |
|---|---|
| Version bump in `Cargo.toml`, `CHANGELOG.md` | release-please (via the release PR) |
| `Cargo.lock` refresh on the release PR | the `sync-lockfile` job in `release-please.yml` |
| The `vX.Y.Z` **tag** | release-please, when the release PR merges |
| The GitHub **release** and its body/notes | release-please creates them as a draft |
| Release **assets** (tarballs + `.sha256`) | `release.yml`, via `gh release upload` |
| Homebrew tap formula | `release.yml` |
| Draft -> published transition | `release.yml`, after all assets are attached |
| `phux-protocol` on crates.io | a human, via `publish-crate.yml` |
| Integration versions | release-please component PRs |
| Integration validation, assets, and publication | `agent-integration-release.yml` |
| Cockpit version and changelog | release-please, under `clients/cockpit` |
| The `cockpit-vX.Y.Z` tag and draft release | release-please |
| Cockpit ZIP, DMG, signature/notarization evidence, and publication | `cockpit-release.yml` |
| `phux-cockpit` Homebrew cask | `cockpit-release.yml` |

`release.yml` never creates a tag, release, or release body. It uploads assets
onto the draft release-please made and only flips that draft to public after the
complete target matrix succeeds, so it cannot clobber the generated changelog or
expose a half-built release. The Homebrew push runs *after* that flip: the tap
validates every push by re-resolving the release through the GitHub API, and a
draft is invisible to it, so a formula pushed before publish is a guaranteed red
tap build.

## Release control surface

| You want to | Do this |
|---|---|
| Ship a release | Mark the open **release-please** PR "Ready for review" (it is born draft; undrafting runs CI), then merge it |
| Prove the release is locally coherent first | `just release-preflight vX.Y.Z` |
| Skip crates.io packaging during a fast/offline binary-only check | `just release-preflight-fast vX.Y.Z` |
| Re-build or re-attach assets for an existing tag | Dispatch **Actions -> release** with `tag=vX.Y.Z` |
| Publish `phux-protocol` to crates.io | Dispatch **Actions -> publish-crate** with `tag=vX.Y.Z`, `dry_run=false` |
| Revalidate an integration tag without publishing | Dispatch **Actions -> Release agent integration** with its component tag and `dry_run=true` |
| Finish an integration release that stalled in draft | Dispatch **Actions -> Release agent integration** with its component tag and `dry_run=false` |
| Re-build or finish a Cockpit release | Dispatch **Actions -> Release Cockpit** with `tag=cockpit-vX.Y.Z` |
| Check Cockpit locally before its release PR merges | `just cockpit-test`, then `clients/cockpit/scripts/package-macos.sh` after `just cockpit-ffi` |
| Ask whether anything is stuck right now | `just release-drift` (needs an authenticated `gh`) |
| Report a hand-recovered release to Linear | Dispatch **Actions -> linear-release** with the tag, `stage=building`, then again with `stage=released` |
| Check a suspected install-doc drift | `bash scripts/check-install-surface.sh` |

## What runs when

| Flow | Trigger | What it does |
|---|---|---|
| Pull request CI | `pull_request` | Docs, Rust, OpenCode V2, Pi, and Claude package gates plus fast real-PTY e2e unless the change is docs-only. Draft PRs skip all of it until marked ready. |
| Cockpit CI | relevant pull request or `main` path | Builds the same-checkout FFI, tests both Cockpit graphs, and compiles the AppKit app on free arm64 `macos-26`. Draft PRs allocate no runner; ZIP/DMG packaging and the three-cycle soak run only on `main` or manual dispatch. |
| Conventional-commit gate | `pull_request` | `commitlint` lints every PR commit and the PR title against `commitlint.config.mjs`. It must be required by main's ruleset; the 2026-09-03 audit found that live setting missing. |
| Main CI | push to `main` | Same gates as PR CI and refreshes warm caches; a narrowly identified release-only merge skips the duplicate compile because its exact tree already passed required PR CI. |
| release-please | push to `main` | Maintains the release PR; on merge, tags `vX.Y.Z`, creates a draft GitHub release, and calls `release.yml`. |
| Release artifacts | called by release-please (or manual dispatch) | Requires all target builds, attaches tarballs + checksums, publishes the complete release, then updates Homebrew. |
| Cockpit release | called by release-please, or manual dispatch | Re-tests the tagged tree, packages, signs and optionally notarizes, verifies downloaded ZIP/DMG assets, proves the Homebrew cask reached the tap, then publishes the draft. |
| Cockpit SDK head | source-repository dispatch, manual, or Monday 07:17 UTC | Builds Cockpit against the exact SDK ref supplied by the fork; the weekly run catches missed dispatches. |
| Crate publish | manual `publish-crate` workflow | `phux-protocol` package dry-run, then publish when `dry_run=false`. |
| Agent integration release | component tag or manual dry run | Re-runs locked gates, creates one checksummed artifact, clean-installs npm artifacts, publishes npm with provenance where applicable, and publishes the component draft release. |
| Stress lane | nightly, manual, or PR label `stress` | Heavy resize/output/lifecycle storms that are useful but too slow for every PR. |
| Release drift | daily at 15:20 UTC, or manual | `scripts/check-release-drift.mjs`. Fails if a release is stuck. See "When a release goes quiet". |
| Linear release report | called by release-please, or manual dispatch | `linear-release.yml`. `stage=building` at tag time, `stage=released` once artifacts are public. Dispatchable so a hand-recovered release can still be reported. |

### Monorepo CI routing

The root workflow always emits its required `check` and `test` contexts, but
its classifier skips both jobs before runner allocation for a positively
identified Cockpit-only diff. Unknown paths fail closed into the root lanes.

| Change | Root Phux CI | Cockpit macOS CI |
|---|---|---|
| `clients/cockpit/**` only | skipped | full tests + app build |
| Cockpit release metadata (`clients/cockpit/**` plus the shared release manifest) | skipped | full tests + app build |
| Root/crate/integration only | full | not triggered |
| `phux-client-core`, `phux-client-ffi`, `phux-perf`, `phux-protocol`, Cargo manifests/lockfile, Rust toolchain, or root Cockpit recipes | full | full tests + app build |
| Draft PR | skipped until ready | skipped until ready |
| Cockpit path on `main` | skipped | tests + app build + package + three-cycle soak |

`scripts/ci/classify-changes.sh` owns the root decision and
`scripts/ci/check-classify-changes.sh` locks its truth table. Keep Cockpit's
workflow `paths` list aligned with the transitive in-repo dependency closure of
`phux-client-ffi`; otherwise a shared ABI input can change without rebuilding
the app that consumes it.

Required secrets:

| Secret | Used by | Required for | Set? |
|---|---|---|---|
| `HOMEBREW_TAP_DEPLOY_KEY` | `release.yml`, `cockpit-release.yml` | Automatic push to `phall1/homebrew-tap`. Root Phux may publish without it; Cockpit fails before asset publication because its cask update is part of the release contract. | yes |
| `CARGO_REGISTRY_TOKEN` | `publish-crate.yml` | Publishing `phux-protocol` to crates.io. Not needed for binary/Homebrew-only releases. | yes |
| `MACOS_CERTIFICATE`, `MACOS_CERTIFICATE_PASSWORD`, `MACOS_SIGNING_IDENTITY` | `cockpit-release.yml` | Optional all-or-nothing Developer ID signing. With none, Cockpit is explicitly ad-hoc signed. | no |
| `APPLE_NOTARY_KEY`, `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID` | `cockpit-release.yml` | Optional all-or-nothing notarization; required whenever Developer ID signing is configured. | no |
| _(none)_ | `agent-integration-release.yml` | Publishing `@phux/*` to npm — uses OIDC trusted publishing, not a secret. See below. | n/a |
There is deliberately **no npm secret**. `agent-integration-release.yml` publishes
through [npm trusted publishing](https://docs.npmjs.com/trusted-publishers), which
exchanges the workflow's OIDC identity for a short-lived registry credential, so
there is no long-lived token to leak, rotate, or find missing at release time.

Three things that lane depends on, all enforced in CI by
`scripts/check-install-surface.sh` so they cannot drift back:

- **The publish job runs on `ubuntu-latest`.** npm rejects OIDC from self-hosted
  runners, and this repo's other jobs run on Blacksmith. The expensive gates stay
  on Blacksmith; only the small publish job is GitHub-hosted.
- **Each package's `repository.url` matches this repository exactly**
  (`https://github.com/no-phux/phux.git`). npm validates it during the token
  exchange and when attaching provenance.
- **No `NODE_AUTH_TOKEN`/`NPM_TOKEN` is wired in.** Its presence would mean the
  lane had silently reverted to a long-lived credential.

**Trusted publishing cannot perform a package's *first* publish.** A trusted
publisher is configured on npmjs.com against a package that already exists, so a
brand-new package has to be bootstrapped once by a human with an authenticated
`npm publish`, after which the trusted publisher is configured and every later
release is hands-off. Budget for that the first time a new `@phux/*` package
ships; it is a one-time cost per package, not per release.

The lane is idempotent: it verifies rather than republishes a version already on
the registry, so re-dispatching a tag is always safe.

## When a release goes quiet

Every release defect this repo has hit failed **silently**, which is why the
drift check exists and why it is worth understanding what it looks for.

`v0.19.0` was prepared and then dropped: release-please's release step aborted
*inside a green run*, so there was no tag, no release, and no artifacts, while
the merged release PR kept its `autorelease: pending` label — which in turn
blocks release-please from opening the *next* release PR. The agent integration
lane failed on all four of its first invocations and left four permanent
0-asset drafts. Nothing was red in any of those cases.

So a release is finished only when all four of these hold, and
`scripts/check-release-drift.mjs` asserts each one:

| Assertion | The failure it catches |
|---|---|
| No release has been a draft longer than the grace window | A publish lane that never attached assets or never flipped the draft |
| No published release has zero assets | A draft flipped public before, or instead of, its upload |
| No merged PR still carries `autorelease: pending` | release-please built no release for a merged release PR |
| Every version in `.release-please-manifest.json` has its tag | A release prepared, merged, and then never cut |

A failing drift run means a release is stuck, not that the check is broken; the
failure message carries the exact dispatch command to unstick it.

Post-release verification:

```sh
scripts/install.sh --dry-run --version vX.Y.Z
brew fetch --formula phall1/tap/phux
cargo search phux-protocol --limit 1
npm view @phux/opencode version
npm view @phux/pi version
claude plugin marketplace list
```

Use the GitHub release page to confirm that the expected target tarballs and
`.sha256` sidecars uploaded. The current release lane builds macOS arm64,
Linux x86_64, and Linux arm64.

## What ships where

| Artifact | Channel | Mechanism |
|---|---|---|
| `phux`, `phux-mcp` binaries | Homebrew + GitHub release | [`release.yml`](../.github/workflows/release.yml), called by release-please |
| `phux-protocol` crate | crates.io | [`publish-crate.yml`](../.github/workflows/publish-crate.yml), manual dispatch only |
| `@phux/opencode` | npm + GitHub release | `opencode-plugin-vX.Y.Z`, [`agent-integration-release.yml`](../.github/workflows/agent-integration-release.yml) |
| `@phux/pi` | npm + GitHub release | `pi-extension-vX.Y.Z`, [`agent-integration-release.yml`](../.github/workflows/agent-integration-release.yml) |
| Claude Code plugin | repository marketplace + GitHub release | `claude-plugin-vX.Y.Z`, [`.claude-plugin/marketplace.json`](../.claude-plugin/marketplace.json) |
| Phux Cockpit | Homebrew cask + GitHub release | `cockpit-vX.Y.Z`, ZIP + DMG + `SHA256SUMS`, [`cockpit-release.yml`](../.github/workflows/cockpit-release.yml) |

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

**This layout is a consumed contract, not just a convention.** `phux update`
(the in-binary self-update path, [ADR-0074](../ADR/0074-self-update-trust-boundary.md))
resolves the release, derives `phux-<tag>-<target>.tar.gz` and its `.sha256`
sidecar from exactly this naming, verifies the digest before unpacking, and
refuses any archive whose members are not precisely the six above. Renaming an
artifact, dropping the sidecar, changing the `"<64 hex>  <archive>"` sidecar
format, or adding a member to the tarball breaks every installed phux's ability
to update itself — silently for the naming, loudly for the members. Change them
together with `crates/phux/src/commands/update/release.rs` and
`crates/phux/src/commands/update/apply.rs`, or not at all.

## Versioning

The workspace shares one `version` in the root `Cargo.toml`
(`[workspace.package]`). All in-repo crates inherit it with
`version.workspace = true`, and internal workspace dependencies use path-only
requirements so release bumps do not require duplicate manifest edits.

Cockpit and the three host integrations intentionally version independently.
Cockpit's version lives in `clients/cockpit/version.txt`, `app.zon`, and
`build.zig.zon`; the Release Please manifest records the latest released
version and `clients/cockpit/scripts/check-release-version.sh` requires all four
copies to agree. Its tags are `cockpit-vX.Y.Z`.

Integration versions live under `integrations/{opencode,pi,claude}` in the
Release Please manifest and package lockfiles; Claude's component also
synchronizes its plugin manifest and repository marketplace entry. Run
`node scripts/check-agent-integration-versions.mjs` after any version-bearing
change. Host APIs and the phux CLI evolve on different schedules, so these
components do not mirror the Rust workspace version.

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

release-please then tags `vX.Y.Z`, creates a draft GitHub release with the
generated changelog as its body, and calls `release.yml`, which validates the
tag against Cargo's resolved versions and builds `phux` + `phux-mcp` for
`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, and
`aarch64-unknown-linux-gnu`, packages `phux-<tag>-<target>.tar.gz` + `.sha256`,
uploads them onto that release, and publishes the draft once every target and
asset is present. Only then — if the `HOMEBREW_TAP_DEPLOY_KEY` secret is set —
does it regenerate and push `Formula/phux.rb` to the tap. A failed tap push no
longer holds the release in draft; the tap's own scheduled update workflow
re-resolves the public release and lands the same formula within fifteen
minutes.

**Backfilling an old tag is safe; it will not move the tap backwards.**
`release.yml` is dispatchable against any existing tag, which is how a release
whose build failed gets its assets attached after the fact. Assets are per-tag,
so re-running an old tag only fills in that release. The tap is not per-tag —
`Formula/phux.rb` is version-pinned and is a single moving pointer — so the
`homebrew` job compares the tag against the version the formula currently serves
and skips the push if it would be a downgrade, emitting a warning annotation
instead. Backfilling `v0.9.0` after `v0.10.0` had shipped is exactly the case
that motivated this; before the guard it silently rewrote the tap to `v0.9.0`.

Release builds use rustup plus the official Zig tarballs instead of the Nix dev
shell, because portable release binaries must not record `/nix/store` dynamic
library paths.

`scripts/check-binary-portability.sh` enforces that on **both** platforms before
packaging: macOS binaries may link only `/usr/lib/**` and `/System/Library/**`,
Linux binaries only the glibc runtime set (`libc`, `libm`, `libgcc_s`, `libdl`,
`libpthread`, `librt`, `libutil`, `ld-linux`). It also fails if a Linux binary
demands a glibc symbol version above `PHUX_GLIBC_MAX` (2.35, the ubuntu-22.04
floor the Linux legs build on), so a runner image bump cannot quietly raise the
minimum distro. Before this existed the check was a `grep` for `/nix/store` in
`otool -L` output — macOS only, one failure mode, and nothing whatsoever on
Linux.

Those tarballs are pinned by SHA-256 in `release.yml`, one digest per target,
and the digests are hand-written on purpose — a checksum fetched at build time
would verify nothing about the server that served the tarball. **Bumping
`ZIG_VERSION` means re-pinning all three digests in the same commit.** Missing
that is what published `v0.10.0` with no assets: the version moved to `0.16.0`
while every digest stayed on `0.15.2`, so all three matrix legs failed at
`shasum -c`, and release.yml runs only after the tag and release already exist.
`just zig-pin-check` (`scripts/check-zig-pins.sh`, a `just ci` and ci.yml step)
compares the pins against `https://ziglang.org/download/index.json` and fails on
a stale one; it skips itself when the index is unreachable.

The latest GitHub release is always the portable public release to point at.
Naming a current version in prose is how the README came to advertise a
long-superseded tag while the repo shipped `v0.7.0`, so don't reintroduce one
here. `v0.0.1` is the single exception worth naming: it was seeded with a Linux
x86_64 tarball plus checksum, but that first artifact is Nix-linked and not
portable, so
do not point installers or the tap at it.

For an emergency host-only artifact, use the same dist layout locally:

```sh
cargo build --locked --release --bin phux --bin phux-mcp
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
unversioned command is user-facing because every current GitHub release is
portable:

```sh
curl -fsSL https://raw.githubusercontent.com/phall1/phux/main/scripts/install.sh | bash
```

Keep it aligned with the release layout above. It should download the target
tarball and `.sha256` sidecar from the selected release, verify the checksum
before unpacking, and install `phux` + `phux-mcp` into
`${PHUX_INSTALL_DIR:-$HOME/.local/bin}`. With no `--version`, it resolves the
current GitHub release. Keep the explicit `v0.0.1` refusal as a historical
safety guard. User-facing docs should point at the latest GitHub release rather
than naming a version, which goes stale the moment the next one ships.

### CPU baseline caveat

`libghostty-vt`'s `build.rs` lets zig auto-detect the host CPU for
native builds, so Linux artifacts may carry instructions specific to the
runner generation and can `SIGILL` on older hardware. `aarch64-apple-darwin`
has a uniform baseline and is unaffected. Pinning Linux CPU baselines through
`libghostty-vt`'s build is future work.

## Cutting a Cockpit release

Cockpit is a Release Please component, not part of the Rust workspace version.
A Cockpit conventional commit updates the shared draft release PR only under
`clients/cockpit` plus the root release manifest. Mark that PR ready, wait for
`cockpit-ci`, `ci` (the aggregate of `check`/`test`), and `commitlint`, then merge it. Release Please
creates `cockpit-vX.Y.Z` and a private draft; `cockpit-release.yml` re-tests the
exact tag, creates the arm64 ZIP and DMG, verifies the downloaded copies and
their `SHA256SUMS`, updates and remotely verifies `Casks/phux-cockpit.rb`,
records signing status in the notes, and only then publishes the draft.

Developer ID and notarization credentials are optional by policy, but never
partial. No Apple secrets means an explicitly ad-hoc-signed release and a cask
that removes quarantine with a caveat. Any Developer ID secret requires all
three signing values and all three notarization values; otherwise the workflow
fails before it uploads or publishes anything.

Recovery is idempotent:

```sh
gh workflow run cockpit-release.yml \
  --repo no-phux/phux \
  -f tag=cockpit-vX.Y.Z
```

The job refuses unexpected assets on a draft and refuses any partial or
unexpected asset set on an already-published release, so a replay cannot
silently replace a public release with different bytes.

## One-time Cockpit import cutover

The imported branch contains a real two-parent merge whose second parent is
Cockpit's rewritten 199-commit history. GitHub's enabled squash/rebase merge
methods would discard that parent, while the required-linear-history rule
rejects an ordinary merge commit. Therefore the migration is reviewed as a PR
but landed once as a non-force fast-forward by an organization administrator
using the ruleset bypass.

1. Add `commitlint` to the live required checks beside `check` and `test` (the
   2026-09-03 audit found it missing). Push the migration branch once and open
   it ready for review; avoid draft and synchronization churn. Wait for all
   root and Cockpit checks on that exact head.
2. Fetch immediately before landing and prove `origin/main` is still the tested
   PR base. If it moved, merge current `main` into the migration branch and
   rerun CI; never rebase or squash the imported graph.
3. With explicit authorization for this upstream operation, fast-forward
   `main` without force:

   ```sh
   git fetch origin main
   head="$(git rev-parse integration/cockpit-monorepo)"
   test "$(git merge-base origin/main "$head")" = "$(git rev-parse origin/main)"
   git push origin "$head:refs/heads/main"
   git fetch origin main
   git merge-base --is-ancestor "$(<.github/cockpit-history-tip)" origin/main
   ```

4. Confirm the canonical Cockpit workflows are visible, then disable the four
   standalone scheduled/tag workflows so one change cannot consume two macOS
   lanes:

   ```sh
   for workflow in ci.yml release-please.yml release.yml sdk-head.yml; do
     gh workflow disable "$workflow" --repo no-phux/phux-cockpit
   done
   ```

5. Do not archive the standalone repository yet. The first canonical
   `cockpit-v*` release must prove the tag, notes, ZIP, DMG, checksums, signing
   status, publication, and Homebrew cask. Then archive the old repository or
   leave it read-only as the pre-monorepo release record.

The first canonical Cockpit release temporarily used a top-level
`bootstrap-sha` at the final filtered standalone tip so it included only
post-cutover work rather than relisting 199 historical commits. That exception
was removed after `cockpit-v0.16.2` published successfully; canonical
`cockpit-v*` tags are now the permanent baseline. Do not reintroduce the
bootstrap setting.

There is no history-removing rollback. Before the fast-forward, stop and fix the
branch. After it, fix forward or land an ordinary forward revert of the visible
integration files; never reset `main`, delete the imported parent, or force-push
the branch, because that would destroy the property this cutover exists to
preserve.

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
