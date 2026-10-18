---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-17
---

# CI

**TL;DR.** One classifier routes every change to the lanes it can affect. The
`ci` aggregate is the merge gate. Caches save on `main` pushes only and are
pruned to one entry per prefix. Main runs reuse a successful run's receipt
only for an identical tree. Release lanes are in [Releasing](./RELEASING.md).

## Runners and concurrency

The repository is public, so standard hosted runners are free. Workflows use
`ubuntu-latest`, `ubuntu-24.04`, `ubuntu-24.04-arm`, and `xcode-27`, with no
larger or self-hosted runners. The account allows 20 concurrent standard
jobs, 5 of them macOS. Concurrency groups use the `mini-v1-` namespace, and
`pr-janitor` cancels a closed PR's live runs.

## Routing

[`scripts/ci/classify-changes.py`](../scripts/ci/classify-changes.py) owns the
routes, and `scripts/ci/detect-changes.py` resolves the diff for pull
requests, pushes, and merge groups. `ci.yml`, `native-setup.yml`,
`web-check.yml`, and `desktop-test.yml` read it through
`.github/actions/classify-changes`, with no outer path filters.
The web and desktop lanes' `crates/*` routes are not listed: the classifier
walks the Cargo manifests from each lane's entry points (`LANE_ROOTS`) and
follows an optional dependency only when an enabled feature turns it on, so
a dependency added or dropped re-routes without a table edit.
`desktop_needed` runs two `xcode-27` jobs and native Ubuntu 24.04 x64/ARM64
jobs for the desktop host's path crates,
the toolchain inputs, the root files the crate embeds, its own workflow, and
unrouted `clients/desktop` paths. `macos` runs `just desktop-lint`,
`just desktop-test`, and `just desktop-perf-budget` on the same test build:
the headless GPUI benchmark's scene and runtime counters must match
`clients/desktop/scripts/perf-budget.ts` exactly, and its tick and RSS
ceilings catch only gross regressions. `package` builds the debug CLI and
`just desktop-package`, then runs `smoke-app.ts` with
`--allow-unconfirmed-present` and uploads the evidence directory. Neither job
holds a build cache, so each run is two cold GPUI and libghostty builds on the
5 macOS slots shared with the release lanes. So `main` keeps one running and
one pending desktop-test run, not one per merge: per-SHA runs once stacked
~35 macOS jobs ahead of the release builds and left drafts unpublished for
hours. A hosted virtual display never
confirms a presented drawable, so real-GPU qualification stays manual:
`just desktop-native-test` on an Apple silicon Mac with a graphical login
requires `confirmed` presentation.
The Linux jobs run the native test/lint gates, build and extract the portable
package, then run `smoke-linux.ts` under Xvfb/lavapipe with an isolated server.
`just desktop-renderer-test` also exercises the pinned wgpu dependency's
submission, shared-atlas and transparent-capture regressions in a disposable
workspace, reusing the native target directory.
That gate targets the low-level `wgpu_` modules, not gpui-fast's opt-in
composed-window tile suite. Four composed-path equivalence failures reproduce
unchanged in pristine gpui-fast 0.1.9 on lavapipe (`phux-9ddi1`); the desktop
does not enable that composition path.
Native frame capture and an independent X11 window screenshot with OCR,
keyboard delivery, clipboard paste, shifted Meta bytes, window identity and
app-crash session survival are checked separately.
Release and CI smoke runs require the renderer's log to identify Vulkan;
silently falling back to OpenGL does not qualify the Vulkan lane.
Submitted receipts never count as confirmed physical-display presentation.
Wayland compositor, physical GPU, IME and screen-reader qualification are not
implied by this software-renderer lane.
Unknown paths, unavailable history, and empty diffs
request every surface. On pull requests the classifier also emits
`test_filterset` (an `rdeps(=crate)` union), so a library change skips
unrelated crates. A `tests/`-only diff builds just that test. Pushes to
`main` run the full pool. A release PR's version-only hunks
(`scripts/ci/release_metadata.py`) skip compile lanes.
`bash scripts/ci/check-classify-changes.sh` tests the routes.

The required `ci` aggregate rejects failed or cancelled lanes. The test
profile compiles once per run: `test build` runs `just test-archive`, which
compiles the union of the unit halves and the e2e set into one nextest archive
(about 410 MB, a one-day artifact). `test`, `test (integration)`, and `e2e`
restore no build cache (`build-cache: none`); they download the archive and run
`just test` with `PHUX_UNIT_PART=lib` or `integration`, or `just e2e`, with
`PHUX_TEST_ARCHIVE` set. A `tests/`-only diff archives that test plus the e2e
binaries when e2e runs. `uniffi` builds its own feature set. Every lane keeps
`--workspace` and `TEST_FEATURES`, so feature unification and the rdeps
filterset match the whole pool.

## Caches

The Actions cache is 10 GB per repository with LRU eviction, and a cache
saved on `refs/pull/N/merge` restores only on that PR. So every cache saves on
`main` pushes only, and pull requests restore without writing:

- The check, test build, and icount lanes keep one mbx `objects` entry each
  (`.github/actions/setup-rust-lane`). The key is lane, `flake.lock`,
  `Cargo.lock`, and the UTC day. A new day restores the previous entry by
  prefix and saves once; a failed main lane still exports when the exact key
  was not restored. The store is collected before compiling, under a 40 GiB
  cap above the largest working set.
- `test build` sets `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0`,
  which keeps its store near 0.7 GB compressed (line tables pushed it to
  6.1 GB and a 6m 28s unpack, run 38503810705). With the store a phux-tui
  change compiles in about 3 minutes; a phux-protocol change still recompiles
  every workspace crate.
- `uniffi` keeps a dependency-only `rust-cache` (`build-cache: deps`) with
  `cache-on-failure`. libghostty-vt-sys downloads a prebuilt engine
  ([SETUP](./SETUP.md)), so no lane compiles Zig for it.
- Release builds cache the crate registry only, never `target/`.

`prune-build-cache` keeps the newest entry per lane and per maintained
prefix, deletes every `sccache/` object, and deletes retired prefixes,
including the old per-lane `v0-rust-phux-test-`,
`v0-rust-phux-test-integration-`, and `v0-rust-phux-e2e-` dependency entries.
`pr-janitor` deletes a closed PR's merge-ref caches.

## Validation reuse

Pull request validation is latest-wins. Main runs are keyed by SHA, so a
later push cannot cancel validation that a release tag needs. A main run may
reuse a seven-day validation receipt only from a successful run in this
repository with the same workflow path, identical routed coverage, and the
same Git tree, resolved independently through GitHub's Git database.
Anything else runs the normal lanes.
