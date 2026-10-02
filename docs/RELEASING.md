---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-27
---

# Releasing

**TL;DR.** Release Please cuts versions, tags, and private draft notes.
`ci` proves the commit. `publish` ships every draft whose tag points at a
green `ci` commit: root binaries, Cockpit, desktop alphas, mobile FFI, and host integrations.
One dispatch finishes a stuck draft. `phux-protocol` on crates.io stays a
separate human dispatch.

## Who owns what

This boundary is load-bearing; blurring it makes two workflows fight over the
same release.

| Thing | Owner |
|---|---|
| Version bump in `Cargo.toml`, `CHANGELOG.md` | release-please (via the release PR) |
| `Cargo.lock` refresh on the release PR | the `sync-lockfile` job in `release-please.yml` |
| The `vX.Y.Z` **tag** | release-please, when the release PR merges |
| The GitHub **release** and its body/notes | release-please creates them as a draft |
| Release **assets** (tarballs + `.sha256`) | `release.yml`, called by `publish.yml` |
| Homebrew tap formula | `release.yml` |
| Draft -> published transition | `publish.yml`, after `ci` is green for that tag |
| `phux-protocol` on crates.io | a human, via `publish-crate.yml` |
| Integration versions | release-please component PRs |
| Integration validation, assets, and publication | `agent-integration-release.yml` |
| Cockpit version and changelog | release-please, under `clients/cockpit` |
| The `cockpit-vX.Y.Z` tag and draft release | release-please |
| Cockpit ZIP, DMG, signature/notarization evidence, and publication | `cockpit-release.yml`, called by `publish.yml` |
| `phux-cockpit` Homebrew cask | `cockpit-release.yml` |
| Desktop version and changelog | release-please, under `clients/desktop` |
| The `desktop-vX.Y.Z-alpha.N` tag and prerelease draft | release-please |
| Desktop ZIP, checksum, runtime qualification, and prerelease publication | `desktop-release.yml`, called by `publish.yml` |
| `PhuxFFI-<tag>.xcframework.zip`, its `.sha256`, and `.provenance` on the root release | `ffi-xcframework.yml`, called by `publish.yml` |
| Moving `next` prerelease (green `main`), CLI and Cockpit | `next-release.yml` |

`release.yml` never creates a tag, release, or release body. It uploads assets
onto the draft release-please made and flips it public only after the complete
target matrix succeeds. The Homebrew push runs *after* that flip, because the
tap re-resolves the release through the GitHub API and a draft is invisible.

The built **source** is the tag; the build **harness** is `main`. `publish.yml`
runs on `workflow_run` and `schedule` events, which always execute the default
branch, so `release.yml` checks out `main` first, runs harness scripts such as
`scripts/ci/setup-linux-release-userspace.sh` from it, and only then detaches
to the tag. A release-infrastructure fix that lands after its tag was cut
therefore applies on the next publish run; re-dispatching **publish** is the
recovery. `scripts/check-release-orchestration.mjs` pins this ordering.

## Release control surface

| You want to | Do this |
|---|---|
| Ship a release | Mark the open **release-please** PR "Ready for review" (it is born draft; undrafting runs CI), then merge it. `publish` runs once `ci` is green for that commit |
| Prove the release is locally coherent first | `just release-preflight vX.Y.Z` |
| Skip crates.io packaging during a fast/offline binary-only check | `just release-preflight-fast vX.Y.Z` |
| Re-build or finish any draft | Dispatch **Actions -> publish** with `tag=vX.Y.Z`, `tag=cockpit-vX.Y.Z`, or a component tag. An empty tag reconciles every ready draft |
| Publish `phux-protocol` to crates.io | Dispatch **Actions -> publish-crate** with `tag=vX.Y.Z`, `dry_run=false` |
| Revalidate an integration tag without publishing | Dispatch **Actions -> Release agent integration** with its component tag and `dry_run=true` |
| Check Cockpit locally before its release PR merges | `just cockpit-test`, then `bash clients/cockpit/scripts/build-phux-artifacts.sh` and `clients/cockpit/scripts/package-macos.sh` |
| Ask whether anything is stuck right now | `just release-drift` (needs an authenticated `gh`) |
| Report a hand-recovered release to Linear | Dispatch **Actions -> linear-release** with `vX.Y.Z` or `cockpit-vX.Y.Z`, `stage=building`, then again with `stage=released` |
| Check a suspected install-doc drift | `bash scripts/check-install-surface.sh` |
| Publish or rebuild the `next` channel | Dispatch **Actions -> next-release** (also runs after green `main` CI) |

## What runs when

| Flow | Trigger | What it does |
|---|---|---|
| Pull request CI | `pull_request`, `merge_group` | Compile-free guards always run; Rust and Node integration lanes run for their dependency inputs. Draft PRs skip until ready. |
| Cockpit CI | shared classifier on PR/main | Builds same-checkout FFI and coordinator, tests Cockpit, and compiles the canonical shipping app on arm64 macOS. ZIP/DMG packaging and the soak run only when `publish` ships a Cockpit tag. |
| Browser CI | shared classifier on PR/main, manual | Node adapters/session tests, shipping package, and three real Chrome canvas/live-server tests. Only engine inputs reproduce the committed WASM binary. |
| Native setup | setup inputs, weekly, manual | Uncached native setup/linker assurance; ordinary Rust source changes use the product lanes. |
| Conventional-commit gate | `pull_request` | `commitlint` lints every PR commit and the PR title. Live rules require `ci` and `commitlint` (verified 2026-09-09). |
| pr-janitor | `pull_request` `closed`, or manual dispatch with a PR number | Cancels the closed PR's still-live runs to free standard/macOS concurrency, then deletes its `refs/pull/N/merge` caches to free the 10 GB repository cap. See "Cache budget". |
| Main CI | push to `main` | Reuses successful same-repository validation only for an identical tree, workflow and routed coverage; otherwise runs the normal lanes. Cheap guards always run. |
| release-please | push to `main` | Maintains the release PR. On merge, creates tags and private drafts. Does not build or publish. |
| publish | `ci` or release-please completed on `main`, daily, or dispatch | Ships drafts whose tag points at a green `ci` run. Deletes drafts older than an already published version of the same component. |
| Release artifacts | called by `publish` | Requires all target builds, attaches tarballs + checksums, publishes the complete release, then updates Homebrew. |
| Cockpit release | called by `publish` | Re-tests the tagged tree, packages, signs and optionally notarizes, verifies downloaded ZIP/DMG assets, proves the Homebrew cask reached the tap, then publishes the draft. |
| Desktop alpha release | called by `publish` | Builds the tagged native addon, compiled app and qualification CLI on Apple silicon; verifies ad-hoc signing, archived-app runtime and installer behavior, then uploads and verifies the ZIP/checksum before publishing as a prerelease, never latest. |
| Cockpit SDK head | manual | Builds Cockpit against an explicitly selected SDK ref. Pinned SDK changes still run ordinary Cockpit CI. |
| Crate publish | manual `publish-crate` workflow | `phux-protocol` package dry-run, then publish when `dry_run=false`. |
| Agent integration release | called by `publish`, or a manual dry run | Re-runs locked gates, creates one checksummed artifact, clean-installs npm artifacts, publishes npm with provenance where applicable, and publishes the component draft release. |
| Stress lane | manual or PR label `stress` | Heavy resize/output/lifecycle storms that are useful but too slow for every PR. |
| Scoped mutation | manual | Bounded Rust or Zig advisory scans; ordinary changed-code checks remain in the product lanes. |
| Release drift | daily at 15:20 UTC, or manual | `scripts/check-release-drift.mjs`. Fails if a release is stuck. See "When a release goes quiet". |
| Linear release report | called by `publish` after a public release, or manual dispatch | `linear-release.yml`. Names the Linear release after the tag and copies the tagged changelog section. Root `vX.Y.Z` goes to pipeline `phux`; `cockpit-vX.Y.Z` goes to `phux-cockpit` (secret `LINEAR_COCKPIT_RELEASE_ACCESS_KEY`). A missing Cockpit key warns and skips; it does not hold the GitHub release in draft. |
| next channel | `ci.yml` success on `main`, one run in flight, pending runs coalesced | Release-profile `phux` + `phux-mcp` for the three portable targets, and an ad-hoc-signed Phux Cockpit when its inputs moved, attached to the moving `next` prerelease. No Homebrew. `phux update --channel next` follows `channel.json`; `install-cockpit.sh --channel next` and the app follow `cockpit-channel.json`. |

### Runners, caches, and concurrency

The repository is public, so standard GitHub-hosted runners are free.
Workflows use only `ubuntu-latest`, `ubuntu-24.04`, `ubuntu-24.04-arm`, and
`xcode-27` (macOS 27 with Xcode 27); no larger or self-hosted runners.

The Actions cache is capped at 10 GB per repository with LRU eviction, and a
cache saved on `refs/pull/N/merge` is only restorable from that PR. So
`rust-cache` saves on `main` only and sccache runs `SCCACHE_GHA_RW_MODE=READ_ONLY`
off `main`; any new PR-lane cache needs the same treatment. The account allows
20 concurrent standard jobs, only 5 of them macOS. `pr-janitor` cancels a
closed PR's live runs and deletes its merge-ref caches. Concurrency groups use
the `mini-v1-` namespace.

Root release targets:

| Target | Standard runner | Build userspace |
|---|---|---|
| `aarch64-apple-darwin` | `xcode-27` | Native Apple-silicon macOS |
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `ubuntu:22.04` container (glibc 2.35) |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `ubuntu:22.04` container (glibc 2.35) |

Linux legs build in an `ubuntu:22.04` container to link against glibc 2.35.
Every leg checks its OS/architecture first, binary portability and smoke
checks run before upload, and every target must succeed before the root draft
is published.

### Monorepo CI routing

`scripts/ci/classify-changes.py` owns the product dependency routes;
`scripts/ci/detect-changes.py` resolves PR merge-base, push and merge-group
events. All four consumers use `.github/actions/classify-changes` without
duplicated outer path filters. Unknown paths, unavailable history and empty
diffs request every surface. On pull requests the same classifier also emits
`test_filterset` (`rdeps(=crate)` union, or empty) so a library change can
skip unrelated crates after the `--workspace` build. A `tests/`-only diff
sets `unit_mode=narrow` and builds that integration test instead. Pushes to
`main` keep the full pool. The required `ci` aggregate rejects failed or cancelled lanes and
retains the visible `check`/`test` job names.

| Change | Product validation |
|---|---|
| Handwritten docs, including Cockpit README | Compile-free guards |
| Workflow/action-only changes | Compile-free workflow/contract guards |
| Root `justfile`, `just/perf.just`, `just/release.just`, `just/mutation.just`, `scripts/check-*`, `scripts/build-ffi-xcframework.sh`, `scripts/export-product-skills.sh`, `scripts/product-skills`, `scripts/skills-package/**`, `.agents/skills/beads/**` | Compile-free guards |
| `just/gates.just`, `just/test.just`, `just/build.just` | Root Rust check/test |
| `just/cockpit.just` | Cockpit tests + shipping app |
| `just/setup.just`, `scripts/doctor.sh`, `scripts/setup-rust.sh`, `scripts/test-dev-setup.sh`, `scripts/native-smoke.sh` | Native setup assurance |
| `integrations/**` | Node integration gates |
| `clients/cockpit/**` source | Cockpit tests + shipping app |
| Browser source | Node/WASM package + Chrome/live-server tests |
| Root crates | Rust + Cockpit coordinator coverage; browser also runs for its Rust/demo-server dependency closure |
| Cargo/toolchain/build inputs | Affected products plus clean native setup assurance |
| `.config/zig-toolchain.json` | Affected products plus byte-identical engine reproduction |
| `scripts/install-zig.sh` | Cockpit + native-setup + engine reproduction (Nix phux lanes use flake Zig) |
| Embedded `.agents/skills/using-phux*/**` | Rust + Cockpit; these versioned project skills are compiled product inputs. Push to `main` also mirrors the allowlisted product skills to `no-phux/skills` (see `.github/workflows/sync-skills.yml`). |

`bash scripts/ci/check-classify-changes.sh` exercises routes and actual event
diffs, including cross-surface renames and the browser server's manifest closure.
The browser shell is opt-in: `nix develop .#browser -c python3 scripts/ci/web-browser.py`.

### Validation and artifact reuse

PR validation is latest-wins. Root and Cockpit main runs are keyed by SHA so a
later push cannot cancel validation that an immutable release tag needs. Root
and browser main checks first look for a seven-day validation receipt. Reuse requires a
successful run in this repository, the same workflow path, identical routed
coverage, and the source run's Git tree independently resolved through GitHub's
Git database. PR head and tested merge trees must agree. Missing, expired,
untrusted, differently scoped or unavailable evidence runs normal validation.

Cockpit main still performs packaging and lifecycle verification, which exceed
PR coverage. Its successful main run saves the two final Rust outputs in an
exact-key cache: clean source tree, compiler versions and Zig executable hash,
platform/CPU, Xcode/SDK, profile and build flags (including native-engine
optimization). Unversioned Ghostty source/system-directory overrides cannot
reuse this cache. Release orchestration waits for that tagged commit's
validation before restoring them. The manifest verifies both artifact hashes;
a miss rebuilds from the tagged checkout. Only successful main validation saves
the shared entry. Dependency caches remain best-effort accelerators.

Signing, packaging, downloaded-byte verification and release lifecycle checks
remain release-owned. The aborting root `release` profile and unwinding Cockpit
`ffi-release` profile are distinct; their binaries are never interchanged.

Required secrets:

| Secret | Used by | Required for | Set? |
|---|---|---|---|
| `HOMEBREW_TAP_TOKEN` | `release.yml`, `cockpit-release.yml` | Token authorized to write `no-phux/homebrew-tap`. Root Phux may publish without it; Cockpit fails before asset publication because its cask update is part of the release contract. | yes |
| `CARGO_REGISTRY_TOKEN` | `publish-crate.yml` | Publishing `phux-protocol` to crates.io. Not needed for binary/Homebrew-only releases. | yes |
| `MACOS_CERTIFICATE`, `MACOS_CERTIFICATE_PASSWORD`, `MACOS_SIGNING_IDENTITY` | `cockpit-release.yml` | Optional all-or-nothing Developer ID signing. With none, Cockpit is explicitly ad-hoc signed. | no |
| `APPLE_NOTARY_KEY`, `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID` | `cockpit-release.yml` | Optional all-or-nothing notarization; required whenever Developer ID signing is configured. | no |
| _(none)_ | `agent-integration-release.yml` | Publishing `@phux/*` to npm — uses OIDC trusted publishing, not a secret. See below. | n/a |
There is deliberately **no npm secret**: `agent-integration-release.yml` uses
[npm trusted publishing](https://docs.npmjs.com/trusted-publishers) (OIDC).
`scripts/check-install-surface.sh` enforces that the publish job runs on
`ubuntu-latest`, that each package's `repository.url` is exactly
`https://github.com/no-phux/phux.git`, and that no `NODE_AUTH_TOKEN`/`NPM_TOKEN`
is wired in. Trusted publishing cannot do a package's *first* publish: a new
`@phux/*` package is bootstrapped once by a human `npm publish`. The lane is
idempotent; re-dispatching a tag verifies rather than republishes.

## Desktop alpha releases

Desktop versions are independent of the CLI and Cockpit. Release Please excludes
`clients/desktop` from the root release and updates its `package.json` and
`CHANGELOG.md` through the Node strategy. `versioning: prerelease`,
`prerelease-type: alpha.1`, and `prerelease: true` are all required: the versioning
strategy advances the numeric alpha counter, while the release flag keeps the
GitHub release out of the stable channel. Desktop uses a separate release PR,
so shipping an alpha does not require releasing unrelated root or Cockpit changes.
Release Please also owns the changelog's formatting; the desktop formatter
excludes that generated file so release PRs do not require manual reformatting.

The initial manifest value is deliberately **`0.0.0`**, Release Please's
never-released sentinel, while the package is already `0.1.0-alpha.1`.
`initial-version: 0.1.0-alpha.1` makes the first release PR replace that sentinel
with exactly `0.1.0-alpha.1`; pre-seeding the manifest with alpha.1 would instead
make Release Please propose alpha.2. The drift guard exempts only the sentinel,
not a missing alpha.1 release. Do not hand-create a tag, add a permanent
`release-as` override, or invent a per-package `bootstrap-sha`.
See the upstream [configuration schema](https://github.com/googleapis/release-please/blob/main/schemas/config.json),
[manifest sentinel handling](https://github.com/googleapis/release-please/blob/main/src/manifest.ts),
and [prerelease strategy](https://github.com/googleapis/release-please/blob/main/src/versioning-strategies/prerelease.ts).

The first train is Apple silicon macOS 27 or later only:

```text
desktop-v0.1.0-alpha.1
  phux-desktop-0.1.0-alpha.1-macos-arm64.zip
    Phux.app/
  SHA256SUMS
```

`publish.yml` still requires green `ci.yml` for the exact tagged commit.
The reusable desktop workflow keeps its harness on `main` and checks out the
immutable tag separately for every product build. The `harness` and `source`
checkouts must be siblings, not nested Cargo workspaces: an outer workspace can
capture GPUI's path dependencies and break workspace inheritance. The lane runs
`just desktop-package` and builds a release-profile CLI from the same source for
runtime qualification.
The native build entry point supports the runner's system Bash 3.2; no newer
Homebrew Bash is required for either production or fixture builds.
The CLI is **not bundled**: users install it separately from
<https://phux.sh/install>, and installing the desktop never upgrades a running
coordinator. The app is explicitly **ad-hoc signed, not Apple-notarized**;
there are no desktop Developer ID or notarization secrets.

Before uploading, the lane verifies `codesign`, extracts the ZIP, and runs
`bun clients/desktop/scripts/smoke-app.ts --app <Phux.app> --phux <same-checkout-binary>`
against isolated server state. The smoke must prove rendered terminal output
and retained sessions across client termination/relaunch; installer transaction
checks run separately. After upload, downloaded checksums and bytes must match
before the API changes `draft` to false, `prerelease` to true, and `make_latest`
to false. This train does not alter the CLI `next` channel or a Homebrew cask.

To recover a draft, dispatch **publish** with
`tag=desktop-v0.1.0-alpha.1` (or its later alpha tag), never the leaf workflow.
The release drift check includes alpha drafts, prerelease status, the expected
versioned ZIP and `SHA256SUMS`.


## When a release goes quiet

Release failures have been silent (an aborted release-please step inside a
green run, 0-asset drafts), so `scripts/check-release-drift.mjs` asserts:

| Assertion | The failure it catches |
|---|---|
| No release has been a draft longer than the grace window | A publish lane that never attached assets or never flipped the draft |
| No published release has zero assets | A draft flipped public before, or instead of, its upload |
| No merged PR still carries `autorelease: pending` | release-please built no release for a merged release PR (this also blocks the next release PR) |
| Every version in `.release-please-manifest.json` has its tag | A release prepared, merged, and then never cut |

A failing drift run means a release is stuck, not that the check is broken; the
failure message carries the exact dispatch command to unstick it.

Post-release verification:

```sh
scripts/install.sh --dry-run --version vX.Y.Z
brew trust --tap no-phux/tap # Homebrew 6+
brew tap no-phux/tap
brew fetch --formula no-phux/tap/phux
cargo search phux-protocol --limit 1
npm view @phux/pi version
claude plugin marketplace list
```

Use the GitHub release page to confirm that the expected target tarballs and
`.sha256` sidecars uploaded. The current release lane builds macOS arm64,
Linux x86_64, and Linux arm64.

## What ships where

| Artifact | Channel | Mechanism |
|---|---|---|
| `phux`, `phux-mcp` binaries | Homebrew + GitHub release | [`release.yml`](../.github/workflows/release.yml), called by [`publish.yml`](../.github/workflows/publish.yml) |
| `phux-protocol` crate | crates.io | [`publish-crate.yml`](../.github/workflows/publish-crate.yml), manual dispatch only |
| `@phux/pi` | npm + GitHub release | `pi-extension-vX.Y.Z`, [`agent-integration-release.yml`](../.github/workflows/agent-integration-release.yml) |
| Claude Code plugin | repository marketplace + GitHub release | `claude-plugin-vX.Y.Z`, [`.claude-plugin/marketplace.json`](../.claude-plugin/marketplace.json) |
| Phux Cockpit | Homebrew cask + GitHub release | `cockpit-vX.Y.Z`, ZIP + DMG + `SHA256SUMS`, [`cockpit-release.yml`](../.github/workflows/cockpit-release.yml) |
| Phux Desktop Alpha | GitHub prerelease + desktop installer | `desktop-vX.Y.Z-alpha.N`, ZIP + `SHA256SUMS`, [`desktop-release.yml`](../.github/workflows/desktop-release.yml); Apple silicon macOS only |
| `PhuxFFI.xcframework` (phux-client-ffi for iOS, simulator, macOS) | GitHub release asset on `vX.Y.Z` | [`ffi-xcframework.yml`](../.github/workflows/ffi-xcframework.yml), called by `publish.yml`; see [PhuxFFI xcframework](#phuxffi-xcframework) |
| `PhuxMobileFFI-<tag>.xcframework.zip` (mobile UniFFI runtime projection plus generated Swift) | GitHub release asset on `vX.Y.Z` | [`ffi-xcframework.yml`](../.github/workflows/ffi-xcframework.yml), built beside the C artifact; see [Mobile UniFFI xcframework](#mobile-uniffi-xcframework) |
| `PhuxMobileFFI-<tag>.android.zip` (same UniFFI surface: Kotlin + arm64-v8a/x86_64 `.so`) | GitHub Actions artifact / release asset | [`ffi-android.yml`](../.github/workflows/ffi-android.yml); phux-mobile fetches at `PHUX_REV` |

`@phux/integration-runtime` is a private implementation module bundled into
the public Pi artifact and bundled into the in-repo OpenCode plugin. It has no tag or independent
publication lane.

Every other crate (`phux`, `phux-core`, `phux-server`, `phux-client`,
`phux-tui`, `phux-config`, `phux-mcp`) is `publish = false`: binary or internal-only.
The installable CLI ships through release artifacts and Homebrew instead of
`cargo install phux`.

Each binary release must produce `phux and phux-mcp artifacts` for every target
that publishes. The tarball layout is:

```text
phux-<tag>-<target>/
  phux
  phux-mcp
  README.md
  LICENSE
  NOTICE
  THIRD-PARTY-NOTICES.md
```

`scripts/pack-release.sh` is the only writer of that member list.
`release.yml`, `next-release.yml`, and `scripts/dist.sh` call it; `--smoke`
runs the binary checks before the tarball is sealed. Homebrew installs both
binaries from the same tarball.

**This layout is a consumed contract, not just a convention.** `phux update`
([ADR-0074](adr/0074-self-update-trust-boundary.md)) derives
`phux-<tag>-<target>.tar.gz` and its `"<64 hex>  <archive>"` `.sha256` sidecar
from this naming, verifies the digest before unpacking, and refuses any
archive whose members differ. Change them together with
`scripts/pack-release.sh`, `crates/phux/src/commands/update/release.rs`, and
`crates/phux/src/commands/update/apply.rs`, or not at all.

The opt-in `next` channel ([ADR-0113](adr/0113-next-release-channel.md))
reuses the same member set (either license layout accepted). GitHub's tag is the moving prerelease `next`;
assets are `phux-next.<sha>-<target>.tar.gz` plus sidecar, and `channel.json`
is the pointer `phux update --channel next` reads. Homebrew stays on stable.
Cockpit rides the same prerelease ([ADR-0138](adr/0138-cockpit-rides-the-next-channel.md)):
`phux-cockpit-next.<sha>-macos-arm64.zip` plus a `.sha256` sidecar in the same
`"<64 hex>  <archive>"` form, with `cockpit-channel.json` as its pointer. Each
product rebuilds only when its own inputs moved since its pointer's SHA, and
`scripts/publish-next-channel.sh [--cockpit]` prunes only that product's
assets.

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
`release-please-config.json`). The same extra-files list rewrites annotated
`PHUX_VERSION` literals in `docs/site/worker/Dockerfile`, which
`just toolchain-check` compares. The `sync-lockfile` job then runs
`cargo update --workspace` in the root and standalone browser workspace on the
same PR, since release-please cannot update lockfiles itself.

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
asset is present. Only then — if the `HOMEBREW_TAP_TOKEN` secret is set —
does it regenerate and push `Formula/phux.rb` to the tap. A failed tap push no
longer holds the release in draft; the tap's own scheduled update workflow
re-resolves the public release and lands the same formula within fifteen
minutes.

**Backfilling an old tag is safe.** `release.yml` is dispatchable against any
existing tag and only fills in that release's assets; the `homebrew` job skips
(with a warning) any push that would downgrade the version-pinned formula.

Release builds use rustup plus the official Zig tarballs instead of the Nix dev
shell, because portable release binaries must not record `/nix/store` dynamic
library paths.

`scripts/check-binary-portability.sh` enforces that before packaging: macOS
binaries may link only `/usr/lib/**` and `/System/Library/**`, Linux binaries
only the glibc runtime set (`libc`, `libm`, `libgcc_s`, `libdl`, `libpthread`,
`librt`, `libutil`, `ld-linux`), and no Linux binary may demand a glibc symbol
version above `PHUX_GLIBC_MAX` (2.35).

The Zig tarballs are pinned by hand-written SHA-256 in
`.config/zig-toolchain.json`, one per target. **Bumping `ZIG_VERSION` means
re-pinning all three digests in the same commit**, or every matrix leg fails
after the tag already exists. `just zig-pin-check` (`scripts/check-zig-pins.sh`)
compares the pins against `https://ziglang.org/download/index.json` and skips
when the index is unreachable.

The latest GitHub release is always the portable public release to point at;
do not name a current version in prose. `v0.0.1` is Nix-linked and not
portable, so do not point installers or the tap at it.

For an emergency host-only artifact, use the same dist layout locally:

```sh
bash scripts/build-release-binaries.sh "$(rustc -vV | sed -n 's/^host: //p')"
just dist vX.Y.Z                       # -> dist/phux-vX.Y.Z-<host>.tar.gz (+ .sha256)
gh release upload vX.Y.Z dist/*        # attach the tarball + checksum
```

Do not use this for normal releases. Do not run a local release build inside
`nix develop`; use a host toolchain plus Zig on `PATH` so the packaged binaries
do not link to Nix-store libraries.

### Required secret

`HOMEBREW_TAP_TOKEN` — a token authorized to write
`no-phux/homebrew-tap`.
Without it the release still publishes; only the automatic formula bump
is skipped (a warning annotation is emitted). The formula itself is
produced by [`scripts/gen-formula.sh`](../scripts/gen-formula.sh), which
emits a stable top-level URL plus overrides only for the targets that actually
built — so a partial-matrix release still yields an installable formula.

The generator emits a fatal `depends_on` guard for every platform with no
artifact (macOS carries `depends_on arch: :arm64`), so an Intel Mac is refused
at install time instead of receiving a binary that cannot exec.

### Curl installer contract

The curl installer is a convenience layer over GitHub release artifacts. The
unversioned command is user-facing because every current GitHub release is
portable:

```sh
curl -fsSL https://phux.sh/install | sh
```

`phux.sh/install` and `phux.sh/install.sh` are `scripts/install.sh` served
verbatim: `docs/site/scripts/sync-docs.ts` copies it into the site's
gitignored `public/` at build time and refuses a script without a `#!/bin/sh`
shebang. `site-deploy.yml` lists `scripts/install.sh` in its path filter, and
the script must stay POSIX `sh`.

The installer downloads the target tarball and `.sha256` sidecar, verifies the
checksum before unpacking, and installs `phux` + `phux-mcp` into
`${PHUX_INSTALL_DIR:-$HOME/.local/bin}`; with no `--version` it resolves the
current GitHub release. Keep the explicit `v0.0.1` refusal as a historical
safety guard.

### CPU baselines

Public native artifacts never inherit the build host's CPU features.
`scripts/build-release-binaries.sh` is the stable and `next` CLI build entry
point: Rust targets `x86-64` on Linux x86_64, `generic` on Linux arm64, and
`apple-m1` on macOS arm64; libghostty's Zig build uses `baseline` on every
target. Cockpit uses `apple-m1` for its Rust coordinator/FFI and `baseline` for
both libghostty and the Zig application. The site worker container uses
`x86-64` for Rust and `baseline` for libghostty. These conservative settings
apply only to shipping profiles; ordinary local Cargo and Cockpit development
builds retain their normal host/toolchain optimization choices.

`bash scripts/check-release-cpu-baselines.sh` rejects an unrecognized target or
any release surface that bypasses those pins. The release matrix additionally
runs `scripts/check-binary-portability.sh` on the resulting executables; on
Linux it rejects ELF notes that declare an x86-64-v2-or-newer ISA requirement.

## Cutting a Cockpit release

Cockpit is a Release Please component, not part of the Rust workspace version.
A Cockpit conventional commit updates the shared draft release PR only under
`clients/cockpit` plus the root release manifest. Mark that PR ready, wait for
`ci` and `commitlint`, then merge it. Release Please creates `cockpit-vX.Y.Z`
and a private draft. Once `ci` is green for that commit, `publish` calls
`cockpit-release.yml`, which re-tests the exact tag, creates the arm64 ZIP and
DMG, verifies the downloaded copies and their `SHA256SUMS`, updates and
remotely verifies `Casks/phux-cockpit.rb`, records signing status in the notes,
and only then publishes the draft.

Developer ID and notarization credentials are optional by policy, but never
partial. No Apple secrets means an explicitly ad-hoc-signed release and a cask
that removes quarantine with a caveat. Any Developer ID secret requires all
three signing values and all three notarization values; otherwise the workflow
fails before it uploads or publishes anything.

Recovery is idempotent:

```sh
gh workflow run publish.yml \
  --repo no-phux/phux \
  -f tag=cockpit-vX.Y.Z
```

The job refuses unexpected assets on a draft and refuses any partial or
unexpected asset set on an already-published release, so a replay cannot
silently replace a public release with different bytes.

The `cockpit-vX.Y.Z` tag shape and the `phux-cockpit-<semver>-macos-arm64.zip`
asset name are a consumed contract: `scripts/install-cockpit.sh` and the
site's Cockpit version badge resolve them directly. Rename either and both
break; `scripts/check-install-surface.sh` pins the three halves together.

## PhuxFFI xcframework

`scripts/build-ffi-xcframework.sh` (`just ffi-xcframework`) builds
`crates/phux-client-ffi` as a static library with `--profile ffi-release`
(the C boundary needs `panic = "unwind"`, so never the plain release profile)
for `aarch64-apple-ios`, `aarch64-apple-ios-sim`, and `aarch64-apple-darwin`,
each slice with the libghostty engine compiled in by `libghostty-vt-sys`, and
wraps them as `PhuxFFI.xcframework` whose headers are `phux/client.h` plus a
`module PhuxFFI` map, so Swift can `import PhuxFFI` directly
([ADR-0133](adr/0133-one-client-runtime-below-every-binding.md)). The Intel
simulator is absent by design: the engine's iOS slices come from ghostty's
arm64-only xcframework path. The script then builds and runs a throwaway
SwiftPM executable against the macOS slice, which proves the module map and
the archive link the way a real consumer uses them.

`ffi-xcframework.yml` runs it on `xcode-27` for every root release
(release-please calls it beside `release.yml`) and on dispatch, uploads the
`PhuxFFI-xcframework` workflow artifact, and with a tag attaches three assets
to that release:

| Asset | Contents |
|---|---|
| `PhuxFFI-<tag>.xcframework.zip` | `PhuxFFI.xcframework` at the archive root, the layout a SwiftPM `binaryTarget(url:checksum:)` expects; `swift package compute-checksum` over the zip equals the sidecar |
| `PhuxFFI-<tag>.xcframework.zip.sha256` | `"<64 hex>  <archive>"`, the same sidecar format as the CLI tarballs |
| `PhuxFFI-<tag>.provenance` | the build's inputs, below |

The provenance file is `key value` lines: `phux-rev` and `phux-tree`
(`clean`, or `dirty` when anything is modified or untracked),
`phux-client-abi-version` from the header, `libghostty-vt-rev` as Cargo
resolved it (cross-checked against the root `Cargo.toml` pin), `ghostty-rev`
from the `-sys` crate's build script, `zig`, `rustc`, `cargo-profile`, `mode`,
`targets`, one `rustflags <target> <flags>` line per slice (the explicit CPU
floor: `apple-a7` device, `apple-a12` simulator, `apple-m1` macOS, which
`scripts/check-release-cpu-baselines.sh` pins beside the other release
lanes), `libghostty-cpu-macos` (`baseline`) and `libghostty-cpu-ios`
(`ghostty-xcframework-targets`, because the iOS emit selects its own platform
targets rather than inheriting `LIBGHOSTTY_VT_SYS_CPU`), `xcode`, `macosx-sdk`,
`iphoneos-sdk`, both deployment floors (26.0, phux-mobile's), and one
`slice-sha256 <identifier> <digest>` per slice. Slice verification also
rejects any member above the floor or carrying the other platform's legacy
`LC_VERSION_MIN_*` marker. phux-mobile's `PHUX_REV` pin should match
`phux-rev` of the archive it links.

The xcframework never gates the draft's publication; `release.yml` owns that,
and the CLI tarballs remain the release contract. A failed or missing build is
repaired without a new release:

```sh
gh workflow run ffi-xcframework.yml --repo no-phux/phux -f tag=vX.Y.Z
```

Uploads use `--clobber`, so a replay against the same tag replaces the three
assets with bytes rebuilt from that exact tag. Dispatching with an empty tag
builds the chosen ref and only uploads the workflow artifact, which is how a
branch is proven before it lands.

Locally the script needs full Xcode with the iOS SDK, rustup, and the pinned
Zig on `PATH`; it scrubs Nix devshell toolchain overrides itself (including
the devshell's `DEVELOPER_DIR`, inherited `RUSTFLAGS`, and the SDK pins), so
it runs the same from a stock shell, from `mise`, or under `nix develop`, and
that scrub is why the "never inside `nix develop`" rule for release tarballs
above does not apply to it. It honours cargo's own target directory
(`CARGO_TARGET_DIR`, `build.target-dir`). Output lands in
`target/ffi-xcframework/Artifacts/`, or under `--out` (a relative path is
taken from the invoking directory). A local build is for proving a change;
the slices a release ships come from `ffi-xcframework.yml`, whose provenance
file is the authoritative record.

## Mobile UniFFI xcframework

`scripts/build-mobile-ffi-xcframework.sh` (`just mobile-ffi-xcframework`)
builds `crates/phux-client-ffi` with `--no-default-features --features
uniffi` for the same three Apple targets. That is the same binding crate the
C xcframework above builds, under its other encoder (ADR-0135): one
`projection/` layer maps `phux-client-runtime` values to the product
vocabulary, and the UniFFI lane lowers it into the object, callback, receipt,
and byte-arena surface consumed by native mobile clients. Its connected path
owns no dial, reconnect, frame pump, or remote engine state machine; the
artifact also keeps an isolated `TerminalEngine` for local playground and
test terminals (ADR-0133; phux-mobile ADR-0031). The archive inside the
bundle is `libphux_client_ffi.a`, the name cargo links it under: the artifact
carries the name of the crate that produced it. The bundle, the `PhuxFFI`
module name, the asset names and the provenance keys are unchanged, so a
phux-mobile re-pin is a `PHUX_REV` bump plus the archive name its artifact
verifiers assert.

The output directory contains `PhuxFFI.xcframework`,
`Generated/PhuxFFI.swift`, and `provenance`. The build reads the UniFFI surface
from the macOS library, assembles the native slices, and runs a throwaway
SwiftPM executable that calls `bridgeReady()`. Provenance records the exact
phux revision and tree, dirty state, engine revision, profile, mode, toolchain,
generated-source digest, and every archive digest. A consumer must verify and
replace all three outputs atomically; generated Swift from one revision must
never load a native slice from another.

`ffi-xcframework.yml` uploads this bundle as the
`PhuxMobileFFI-xcframework` workflow artifact. For a root release it also
attaches `PhuxMobileFFI-<tag>.xcframework.zip`, its SHA-256 sidecar, and its
provenance sidecar. The zip contains the xcframework, generated Swift, and
provenance at its root. An empty-tag dispatch proves an exact branch or commit
without mutating a release. The mobile repository resolves the workflow run by
its pinned phux commit and can fall back to building this script from an exact
phux checkout after run-artifact retention expires.

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

## Local fallback: dsr

When Actions is throttled or queued, an operator can run `dsr`
([phall1/doodlestein_self_releaser](https://github.com/phall1/doodlestein_self_releaser))
to replay `release.yml`'s build steps locally (`act`/Docker for Linux, bare
metal for macOS) and `gh release upload` the tarballs and sidecars onto the
existing draft. It reads `release.yml` directly, so nothing here needs syncing;
its target configuration lives in `~/.config/dsr/`. It never publishes the
draft, touches crates.io, or updates the tap: publish the draft first
(`gh release edit vX.Y.Z --draft=false`), then let the tap's scheduled update
land the formula or run `bash scripts/gen-formula.sh` and push it by hand.
`dsr check no-phux/phux`, `dsr build phux --targets linux/amd64`,
`dsr release phux --version vX.Y.Z`, and `dsr fallback phux --version vX.Y.Z`
are the entry points; builds refuse a dirty tree without `--allow-dirty`.

## Installing from the tap

```sh
brew install no-phux/tap/phux
```

The tap does not add Windows support; Windows is not supported here. A Windows
release would need a separate design and build lane rather than a formula tweak.
