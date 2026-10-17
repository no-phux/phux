---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-16
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
requests, pushes, and merge groups. `ci.yml`, `native-setup.yml`, and
`web-check.yml` read it through `.github/actions/classify-changes`, with no
outer path filters. Unknown paths, unavailable history, and empty diffs
request every surface. On pull requests the classifier also emits
`test_filterset` (an `rdeps(=crate)` union), so a library change skips
unrelated crates. A `tests/`-only diff builds just that test. Pushes to
`main` run the full pool. A release PR's version-only hunks
(`scripts/ci/release_metadata.py`) skip compile lanes.
`bash scripts/ci/check-classify-changes.sh` tests the routes.

The required `ci` aggregate rejects failed or cancelled lanes. `test`,
`test (integration)`, `e2e`, and `uniffi` are the test profile's lanes. Each
keeps `--workspace`, so feature unification matches the whole pool.

## Caches

The Actions cache is 10 GB per repository with LRU eviction, and a cache
saved on `refs/pull/N/merge` restores only on that PR. So every cache saves on
`main` pushes only, and pull requests restore without writing:

- The check and icount lanes keep one mbx `objects` entry each
  (`.github/actions/setup-rust-lane`). The key is lane, `flake.lock`,
  `Cargo.lock`, and the UTC day. A new day restores the previous entry by
  prefix and saves once. The store is collected before compiling, under a
  40 GiB cap above the largest working set.
- The test lanes keep a dependency-only `rust-cache` (`build-cache: deps`)
  with `cache-on-failure`, and build with `CARGO_INCREMENTAL=0` and
  `CARGO_PROFILE_DEV_DEBUG=0`. libghostty-vt-sys downloads a prebuilt engine
  ([SETUP](./SETUP.md)), so no lane compiles Zig for it.
- Release builds cache the crate registry only, never `target/`.

`prune-build-cache` keeps the newest entry per lane and per maintained
prefix, deletes every `sccache/` object, and deletes retired prefixes.
`pr-janitor` deletes a closed PR's merge-ref caches.

## Validation reuse

Pull request validation is latest-wins. Main runs are keyed by SHA, so a
later push cannot cancel validation that a release tag needs. A main run may
reuse a seven-day validation receipt only from a successful run in this
repository with the same workflow path, identical routed coverage, and the
same Git tree, resolved independently through GitHub's Git database.
Anything else runs the normal lanes.
