---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-18
---

# CI

**TL;DR.** One workflow, `ci.yml`, validates every pull request and every
`main` push. Its `plan` job routes the diff to lanes, and each lane runs the
same just recipes that `just check` runs on your machine. Only what your
change can affect runs, and anything that runs gates the merge. Post-merge
work follows a green `main`: `release.yml` and `next.yml`. Releasing is
[its own page](./RELEASING.md).

## Workflows

| Workflow | Fires on | Does |
|---|---|---|
| `ci.yml` | every non-draft PR, every `main` push, dispatch | routed lanes and the required `ci` aggregate; on a green push, republishes changed docs and skills |
| `commitlint.yml` | PRs, including title edits | lints every authored (non-merge) commit and the PR title |
| `release.yml` | green `main` ci, daily, dispatch | release PRs, then publishes every draft whose commit is green |
| `next.yml` | green `main` ci, dispatch | the moving `next` prerelease |
| `build-cli.yml`, `desktop-release.yml` | called by the two above | release builds |
| `ffi-xcframework.yml`, `ffi-android.yml` | `main` pushes touching the binding closure; tag dispatch | mobile artifacts. phux-mobile finds them by these file names and push runs, so do not rename or merge them |
| `agent-integration-release.yml` | dispatched by `release.yml` | npm trusted publishing trusts this file name |

The ruleset requires two contexts: `ci` and `commitlint`. A ruleset matches a
job's display name, not its id.

## Lanes

[`tools/src/routing/routes.ts`](../tools/src/routing/routes.ts) maps paths to
surfaces, and [`plan.ts`](../tools/src/routing/plan.ts) maps surfaces to
lanes and lanes to recipes. The web and desktop lanes route the Cargo
closure their builds compile, read from the manifests with features resolved,
so a dependency added or dropped reroutes on its own. A path no route names
requests every lane, so an unknown input fails closed. A change to `ci.yml` or `.github/actions/` reruns
every lane too. Maintainer-only `.beads/` changes run guards, not product
builds. Regenerable `.pi/fabric/` state is ignored so it cannot widen a local
`just check` plan.

| Lane | Job | Recipe | Runs when |
|---|---|---|---|
| guards | `guards` | `just guards` | always: workflows, shell, tools tests, docs, installers, pins |
| lint | `rust lint` | `just rust-lint` | Rust inputs |
| tests | `rust test build`, then `rust tests` | `just test-archive`, `just test` | library or `tests/` changes |
| e2e | `rust e2e` | `just e2e` | as above, unless a `tests/`-only diff touches no e2e binary |
| ffi | `rust ffi` | `just binding-uniffi-test`, `just ffi-abi` | the binding crate's dependency closure |
| icount | `rust icount` | `just perf-icount` | hot-path crates and the baseline ([performance CI](./performance-ci.md)) |
| integrations | `agent integrations` | `just agent-integrations-check` | `integrations/` and plugin manifests |
| web | `web` | `just web-check` | the browser clients and the Cargo closure they build |
| desktop | `desktop (macOS)`, `desktop (Linux)` | `just desktop-lint`, `desktop-tests`, `desktop-check`, `desktop-framework-check debug` | the desktop host and the Cargo closure it builds; PRs only |
| native | `native setup` | `just native-smoke` | toolchain, setup, and link inputs on a clean runner |

On a pull request, a library change runs only its reverse dependencies
(`rdeps(=crate)`), and a `tests/`-only change builds only that test. A release
PR's version-only hunks skip every compile lane. A `main` push runs the full
Rust pool, because two green PRs can still merge into a red tree. The desktop
lane is the exception: on `main`, `next.yml` qualifies every desktop change on
the same macOS and Linux targets, and per-merge macOS jobs once starved the
five-slot macOS pool the release builds share.

`just check` runs the pull-request plan for your working tree against
`origin/main`, through the same recipes. `--dry-run` prints the lanes without
running them. icount (x86_64 valgrind) and native (a clean machine) run only
in CI.

The test profile compiles once per run: `rust test build` archives the unit
and e2e binaries (about 410 MB, kept one day), and `rust tests` and `rust e2e`
download it and compile nothing.

Desktop PRs package and smoke-test a production-feature debug binary instead
of compiling an additional optimized copy. The same packaging and runtime
checks run; `desktop-release.yml` still builds and qualifies optimized binaries
on macOS and both Linux architectures before publication. Native setup and
Linux test orchestration are shared by the PR and release jobs.

## Cancellation and reuse

Pull requests are latest-wins, and closing one cancels its in-flight run.
Every `main` SHA keeps its own run, because `release.yml` publishes a draft
only on a green push run for that exact commit.

When a pull request passes, the `ci` job uploads a receipt: the tree and the
lanes that ran. A `main` push whose tree is identical, with the same lanes,
skips every lane and goes green in seconds
([`receipt.ts`](../tools/src/routing/receipt.ts)). A receipt is metadata,
bound to its run's commit through GitHub's Git database. Any API failure
counts as a miss.

## Caches

The Actions cache is 10 GB per repository with LRU eviction, and an entry
saved on a PR's merge ref restores only on that PR. So caches save on `main`
pushes only.

- The lint, test build, and icount lanes keep one mbx `objects` entry each
  (`.github/actions/setup-rust-lane`). The key is the lane, `flake.lock`,
  `Cargo.lock`, and the UTC day. A new day restores the previous entry by
  prefix.
- The test build sets `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0`,
  which keeps its store near 0.7 GB compressed.
- The ffi lane keeps a dependency-only `rust-cache`.
- Release builds cache the crate registry only, never `target/`.

`prune-build-cache` keeps the newest entry per prefix after each `main` push.

## Runners

The repository is public, so standard hosted runners are free:
`ubuntu-latest`, `ubuntu-24.04`, `ubuntu-24.04-arm`, and `xcode-27`. The
account allows 20 concurrent jobs, 5 of them macOS. The tests in
`tools/src/policy/` and `tools/src/routing/workflows.test.ts` hold the
workflows to these rules, and they run in `just guards`.

## Off CI

`just stress` (CPU-starved on two-core runners), `just mutation-rust`, and
real-GPU desktop qualification (`just desktop-native-test`) run locally.
