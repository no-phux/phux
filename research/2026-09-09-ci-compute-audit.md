---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-09
---

# CI and release compute audit

**TL;DR.** The expensive shape is repeated build ownership across PR, main,
and release, plus executable/test artifacts that the caches do not retain.
Three release platforms are legitimate distribution targets. Rebuilding the
same application graph at several lifecycle stages is the stronger optimization
opportunity. This audit maps the boundaries, measures representative jobs, and
identifies changes that need experiments before claiming savings.

## Evidence and scope

Audited configuration: `6bb68787747fb196299a274a3676c3f346802fe0`.
The linked release run builds `1567bfd0f981e94b8fe745beb2b68b100469edea`
(`chore: release main (#545)`). The relevant workflows, Cargo profiles, Cockpit
build graph and packaging script are identical between these revisions.

Measurements below are job/step wall time, not CPU time, billed minutes, or
fleet-wide averages. Overlapping job durations must not be added to predict
elapsed time. The release was initially in progress; final jobs and the exact
Cockpit job log became available before handoff. Its earlier HTTP 404 log
response was an active-job limitation, not missing evidence in the final audit.

| Evidence | Run / job | Observation |
|---|---|---|
| Linked Cockpit release | [34314197101 / 102346960565](https://github.com/no-phux/phux/actions/runs/34314197101/job/102346960565) | Job 545s, failed; FFI + CLI 287s; tests 76s; packaging 118s; failing soak 20s |
| Same release, macOS CLI | [34314197101 / 102347010389](https://github.com/no-phux/phux/actions/runs/34314197101/job/102347010389) | Job 226s; release build 193s |
| Same release, Linux x64 CLI | [34314197101 / 102347010336](https://github.com/no-phux/phux/actions/runs/34314197101/job/102347010336) | Job 238s; release build 200s |
| Same release, Linux ARM CLI | [34314197101 / 102347010332](https://github.com/no-phux/phux/actions/runs/34314197101/job/102347010332) | Job 555s; release build 505s |
| Same SHA, native setup | [34314197085 / 102346863503](https://github.com/no-phux/phux/actions/runs/34314197085/job/102346863503) | Job 205s; native smoke 177s |
| Recent Cockpit PR | [34313358398 / 102344443309](https://github.com/no-phux/phux/actions/runs/34313358398/job/102344443309) | Job 467s; FFI + CLI 268s; test step 54s; app build 98s |
| Recent Cockpit main | [34313323800 / 102344341326](https://github.com/no-phux/phux/actions/runs/34313323800/job/102344341326) | Job failed after 627s; FFI + CLI 285s; tests 56s; app build 103s; package step 135s |
| Recent root main test | [34313323783 / 102344343753](https://github.com/no-phux/phux/actions/runs/34313323783/job/102344343753) | Job 650s; setup 87s; test/build step 540s |
| Same root main check | [34313323783 / 102344343736](https://github.com/no-phux/phux/actions/runs/34313323783/job/102344343736) | Job 308s; setup 91s; integration gates 81s; Rust checks 71s |
| Cockpit-only merge routing | [34313942107](https://github.com/no-phux/phux/actions/runs/34313942107) | Root check/test skipped; classifier 8s and aggregate 5s |

The recent root and Cockpit-main samples are commit `6319330a`, which changed
Cockpit, Rust tests, scripts, justfile, and docs. They are **not** evidence that
a workflow-only PR ran the whole workspace. The Cockpit PR sample is `bfa450f0`.
The failed Cockpit-main package step built successfully, then its internal
lifecycle probe failed with `startup timed out with 0 direct and 0 recorded
shells`; its duration is not a successful packaging baseline.

The linked release subsequently failed its separate soak with `startup timed
out (coordinator=8029 recorded=0, expected 1 shells)`, after successful packaging.
Root CLI assets published successfully. The four heavy release jobs consumed
**1,564 runner-seconds (26m04s)** combined; that is not 26 minutes of elapsed
time or a dollar bill. The Linux ARM leg was the root-release critical path.
Its slower build is an observation; these logs do not isolate a CPU, cache, or
linker cause, and it is not duplicate architecture coverage.

## Current execution map

```text
Ready PR / synchronize
  root ci -> classify -> check + test -> aggregate ci
  Cockpit paths -> classify -> macOS FFI + CLI -> Zig tests -> app
  native-setup paths -> native Linux setup smoke
  web paths -> WASM engine/adapters/package checks
  conventional-commits -> commit metadata

Merge / push main
  root ci again (Cockpit-only routing can skip Rust)
  relevant Cockpit/native/web workflows again
  relevant site / worker deployment
  release-please -> maintain release PR + sync lockfile
    if a release PR merged:
      root tag -> macOS arm64 + Linux x64 + Linux arm64 CLI builds
               -> attach -> publish -> Homebrew / Linear promotion
      Cockpit tag -> FFI + CLI -> Zig tests -> package/sign/verify/soak
                  -> assets -> Homebrew -> publish
      integration tags -> component npm gates -> pack/publish

Closed PR -> janitor cancels leftover PR runs
Scheduled / explicit -> stress, mutation, SDK HEAD, drift, production probes
```

Release Please's root and Cockpit reusable workflows are gated by their
individual `release_created` outputs. They do not build on every main push.
Root release has exactly three architecture/OS legs, not three repetitions of
the same artifact (`.github/workflows/release.yml:119-218`).

### All 21 workflow files

| Workflow file | Trigger / role | Build boundary |
|---|---|---|
| `ci.yml` | PR, main, merge group | Root lint/doc/dependency checks and workspace tests |
| `cockpit-ci.yml` | Relevant PR/main paths, manual | macOS FFI, coordinator, Zig tests/app; additional main/manual packaging |
| `native-setup.yml` | Relevant PR/main paths, manual | Native Linux smoke without Nix |
| `web-check.yml` | Relevant PR/main paths, manual | Pinned WASM engine regeneration and browser adapter checks |
| `conventional-commits.yml` | PR, merge group | Metadata only |
| `pr-janitor.yml` | PR closed | Cancellation API only |
| `stress.yml` | Schedule, manual, PR label | Ignored real-server stress coverage |
| `mutation.yml` | Monthly, manual | Separate advisory Rust/Zig mutation scans |
| `cockpit-sdk-head.yml` | Weekly, repository/manual dispatch | Consumer coverage against a different SDK revision; two provider graphs |
| `release-please.yml` | Main | Metadata, lockfile resolution, conditional release fan-out |
| `release.yml` | Reusable, manual | Three native CLI distribution targets |
| `cockpit-release.yml` | Reusable, manual | macOS app distribution |
| `agent-integration-release.yml` | Component tags, manual | Exact component package validation/publication |
| `publish-crate.yml` | Manual | Protocol crate dry-run/publication |
| `linear-release.yml` | Reusable, manual | Release tracking API, no product compile |
| `release-drift.yml` | Daily, manual | Release-state inspection, no product compile |
| `site-deploy.yml` | Docs/ADR main paths, manual | Astro site build/deploy |
| `site-deploy-worker.yml` | Worker/edge main paths, manual | Worker validation/deploy and native container |
| `site-native-monitor.yml` | Six-hourly, manual | Live wire probe |
| `site-native-control.yml` | Manual | Worker configuration deployment |
| `site-rollback-worker.yml` | Manual | Worker rollback and authentication probe |

The worker's header says no Rust toolchain is needed in CI, but its Dockerfile
contains a Rust/Zig source build (`docs/site/worker/Dockerfile:2-48`). That is a
separate pinned old revision, with its own Docker layer cache, not reuse of the
current CLI release. Container layer hit rates were not measured here.

### What Cockpit's long target list actually means

The sampled PR reports **52 Zig steps, 16 Zig compile nodes, eight test
executables, and 538 tests** (536 passed, two skipped). Six compile nodes are
cached. These are different counts: generated files, options, tools, execution
and verdict steps are not additional full application builds. Summary entries
marked `(reused)` reference work already in the graph.

| Test root | Cases | Reported compile time |
|---|---:|---:|
| Generated SDK app root | 0 | 1s |
| Shipping `phux-cockpit-extension` | 98 | 18s |
| `cockpit-native-engine-regressions` | 367 | 16s |
| Phux transport | 7 | 1s |
| Phux host | 34 | 2s |
| Phux provider | 9 | 2s |
| Phux pointer | 5 | Cached |
| Phux extension | 18 | 1s |

Those seven nonempty roots collect distinct test-bearing relative-import
closures. Shared named-module imports do not automatically rerun the imported
module's tests. Deleting the five provider roots would silently lose coverage,
not simply remove repeated tests. Reachable production/runtime code can still
be compiled into multiple executable roots.

The graph also compiles a non-test entry-point analysis object (10s), a reflected
model-contract tool (1s), generators and small markup objects. The pinned SDK is
[`34cc9d55`, `build/app.zig`](https://github.com/phall1/native/blob/34cc9d5571599d5ea4feafc9260f36575e67e77b/build/app.zig);
see lines 1874-1961. Cockpit adds its roots in `clients/cockpit/build.zig:114-197`
and `:407-460`. Separate entry analysis is useful for standalone `zig build
test`, which does not analyze the real `main`; it could be optional in a combined
gate that already builds the app. The zero-test root is a small cleanup target.

The shipping app's app-code object, markup-data object and final executable
are likewise not three whole app builds: the final executable was a 379ms
link-shaped step while its app-code object was reported as 1m.

## Findings

Priority: repair dependency-routing holes together with overbroad routing;
then reduce build ownership and executable-target cost. Small repeated shell
commands are secondary. The numbered findings below group related evidence,
not estimates of recoverable runner-seconds.

### 1. Cache hit percentage hides the root test artifact cost

The root-main sample logs `Finished test profile ... in 5m 13s`, then starts
4,360 tests across **96 binaries**. sccache reports 16 hits, zero misses, and
112 non-cacheable calls (108 reason `crate-type`). Its 100% hit rate is for the
16 eligible calls, not all 128 requests. It does not mean the job avoided
compilation/linking of executable targets.

The later ignored-test passes reuse the same build: **0.38s and 0.30s** Cargo
times. Combining or deleting those passes would not recover the initial 313s.
Likewise, printing `Compiling phux-*` does not by itself prove a cache miss:
sccache sits behind those Cargo messages.

**Recommended experiment:** inventory executable/test targets and consolidate
small integration-test roots into suite binaries where process isolation is
unnecessary. Retain nextest test-level isolation, ignored filters, and test
names/coverage accounting. Compare private-target-directory build timings and
test inventories before/after. Also evaluate exact-input executable artifacts;
do not blindly enable every workspace cache and assume correctness or savings.
Historical tracking already exists in `phux-mmxz`; it needs the current
96-binary evidence rather than its old two-core/~40-binary assumptions.

### 2. Cockpit rebuilds its Rust substrate at each lifecycle stage

Cockpit CI and release both run:

```sh
cargo rustc --locked --profile ffi-release -p phux-client-ffi --lib --crate-type staticlib
cargo build --locked --profile ffi-release -p phux
```

The PR sample restored ~591 MiB of Rust cache and ~579 MiB of Zig cache, then
spent 66s on FFI and 201s on the CLI. Its Rust cache explicitly reports
`cache-workspace-crates: false`. These caches reuse dependencies; they are not
a handoff of the completed same-SHA FFI/CLI pair.

The linked release repeats this stage for 287s while the root macOS release
builds `phux` and `phux-mcp` for 193s on another runner. This is repeated source
work, **not interchangeable existing artifacts**: `release` aborts on panic,
whereas `ffi-release` unwinds (`Cargo.toml:240-246`), and the FFI and CLI select
different feature graphs. The FFI's unwind boundary is load-bearing.

There is duplication **inside** the Cockpit stage too: both commands compile
`libghostty-vt-sys`, `libghostty-vt`, protocol and client-core in the sampled log.
Both use `ffi-release`, so panic strategy does not explain this pair. The CLI
enables protocol's `server` feature, adding Ghostty `kitty-graphics`/`png`, while
FFI enables client-core's `native-engine` without protocol `server`
(`crates/phux-client-ffi/Cargo.toml:18-22`,
`crates/phux-protocol/Cargo.toml:17-23`). Separate Cargo resolutions produce
different dependency units, including separate native build output directories.

**First experiment:** resolve FFI and CLI in one Cargo invocation. A plain
combined `cargo build -p phux-client-ffi -p phux --profile ffi-release` also emits
all three FFI crate types declared in its manifest, whereas the current
`cargo rustc` selects only staticlib. Measure avoided dependency/native work
against extra final-output work; do not replace the commands without verifying
the FFI containment contract and resulting artifact set. The CLI adds legitimate
server/TUI dependencies, so its entire 201s is not removable duplication.

**Recommended design:** give the same-checkout macOS FFI/coordinator artifacts
one producer per exact input set. Downstream tests and packaging consume that
producer's verified outputs. Keep the three native release platforms. Sharing
the root release CLI with Cockpit additionally requires a deliberate profile
decision and matching source identity, including Cockpit-only releases.

### 3. Main and release are independent build owners, not an artifact pipeline

`release-please.yml:265-285` starts both artifact workflows immediately after
release metadata, independently of `ci.yml` and `cockpit-ci.yml`. The release
header's claim that Cockpit CI "just built" the same revision is not an ordering
guarantee: the producers can run concurrently, and caches save at job end.
Even a perfectly matching key cannot restore a future cache entry.

There is a root release-metadata skip, but it does not recognize this release:
`ci.yml:179-187` requires a subject shaped `chore(main): release X.Y.Z (#N)`
and exactly four root files. The linked commit is `chore: release main (#545)`
and changes eight files, including Cockpit's release metadata. Both conditions
miss the current multi-component release shape, and full root CI runs alongside
the release builds. Replace this brittle historical convention with verified
release identity/input classification; a subject match alone is not proof that
an exact tree passed required PR checks.

Cockpit main also builds a production app and subsequently invokes packaging.
The sample proves two expensive app-code compilations: **ReleaseFast,
`native-macos.11.0`**, then **ReleaseSafe, `aarch64-macos.11.0`**. The standalone
app step takes 103s. The SDK defaults app optimization to ReleaseFast while
`package-macos.sh:9-10,91-99` explicitly selects ReleaseSafe and ARM64.
Tests default to Debug; changing their mode is a separate coverage decision.

**Immediate consolidation candidate:** one canonical shipping target, CPU and
optimization option set for app/package consumers, or let packaging own the
shipping compile on main. Aligning optimization alone is insufficient if target
selection still differs. Preserve Debug tests and verification of packaged bytes.

**Recommended design:** PR tests validate changes; main produces an exact-SHA
candidate when product inputs change; release consumes a matching candidate or
performs a single fallback build. Signing, archive verification and packaged
lifecycle checks remain attached to the bytes that ship. Do not promote a
cached PR merge-ref artifact as a release-tag artifact without proving identity.

### 4. Cancellation policy protects old running CI without preserving all SHAs

The root, Cockpit, web and native workflows cancel only PR runs. With their
current concurrency groups, GitHub's default one-running/one-pending behavior
still replaces older **pending** main runs. Thus the comment that preserving
running main jobs guarantees every integration point is checked is too strong.

This happened in the sampled burst: Cockpit main run `34313942046` was running,
`34314196959` was cancelled, and the newer `34314412100` was pending. The snapshot
is consistent with pending-run replacement; it does not identify the cancellation
actor. It demonstrates that the observed run history is not all-green/all-tested.

**Recommendation:** latest-wins for superseded validation-only main runs, with
separate, non-cancelling publication groups keyed by release identity. This
optimizes burst behavior; it does not eliminate sequential PR/main repetition.
If per-commit evidence is actually required, design a real queue rather than
relying on `cancel-in-progress: false`.

### 5. Comments and tracking no longer describe the actual compute fleet

`ci.yml` still describes free GitHub ARM runners; its executing lanes are
Blacksmith ARM. `cockpit-release.yml:48-50` says standard free `macos-26`, then
selects `blacksmith-6vcpu-macos-26`. The live jobs confirm those labels. Old
Beads cost claims therefore cannot justify today's spend. Billing was not
queried, so this report assigns no dollar savings.

The existing `phux-7k11` acceptance criterion says main should not repeat full
CI, but the current workflows do repeat relevant validation. This is an
unfinished policy decision, not a missing YAML concurrency feature.

### 6. Routing spends on unrelated surfaces while missing bundled dependencies

The classifier and its existing truth table pass. Direct probes nonetheless
produce the following undesirable policy outcomes:

| Changed path / surface | Current work requested |
|---|---|
| `docs/RELEASING.md` | Two root Rust-configured runners; no Rust compile; all integration gates |
| `clients/cockpit/README.md` | Full Cockpit macOS lane |
| `clients/phux-web/src/lib.rs` | Root check/test plus web build |
| `integrations/pi/src/index.ts` | Root check/test, including all three integrations |
| `crates/phux-server/src/lib.rs` | Root check/test and cold native smoke; Cockpit macOS incorrectly skipped |
| Action-pin-only edit in `release.yml` | Root check/test, native, Cockpit, web: potentially five compiling jobs |

Sources: `scripts/ci/classify-changes.sh:20-80`,
`scripts/ci/check-classify-changes.sh:22-41`, `ci.yml:248-275,365-407`.
The root workspace excludes the browser workspaces (`Cargo.toml:3-9`), and
the Node integrations have their own scoped gates. Their edits need explicit
ownership rather than a fallback into every native phase.

The docs case is a **setup** defect: both jobs install/realize the toolchain
and restore Rust caches before skipping Rust commands. The `test` job does no
product tests; the `check` job still runs all integrations unconditionally.
The measured full-build setup was 87s/91s and integration gates 81s, illustrating
the scale of those stages, not a measured docs-only saving.

The `release.yml` exception exists because that orchestration file also owns
Zig compiler archives/digests. It makes no distinction between a compiler change
and an action SHA edit. Dependabot groups all action updates
(`.github/dependabot.yml:7-14`), so the exception can defeat the grouped-update
fast path. Dedicated native/web workflow edits also run their own heavyweight
lanes even when root CI takes its workflow-only path.

More seriously, Cockpit's outer trigger includes `crates/**` to cover its bundled
coordinator, but the inner classifier recognizes only four shared crates.
Server, CLI, config, TUI, and other coordinator dependencies set
`cockpit_needed=false` (`classify-changes.sh:57-71`). The existing truth table
even expects that for server source. The bundled CLI's compiled `skills/**`
inputs and the classifier script itself are absent from Cockpit's outer paths.
Empty diffs also leave Cockpit false despite the workflow's fail-closed comment.

**Recommended change:** one tested surface/dependency map used consistently by
outer triggers, inner classification, and aggregate checks. Give docs, Node
integrations, browser adapters, native Rust, and Cockpit explicit ownership.
Model the bundled coordinator separately from the FFI subset. Verify skipped
as well as enabled jobs against actual dependency inputs; a passing test of an
outdated routing policy is not a correctness proof.

### 7. Clean native setup coverage runs at ordinary source-change frequency

`native-setup.yml:6-35` matches all `crates/**`, including ordinary source,
tests and crate README changes. The job deliberately has no target cache
(`:54-55`) and builds core/protocol tests plus both CLIs
(`scripts/native-smoke.sh:7-11`). It runs on qualifying PRs and again on main.

The linked release's native smoke cost 177s alongside the Nix ARM lanes and
release targets. Native x64 coverage is valid; making every implementation
edit pay for clean contributor setup is a policy choice with substantial cost.

**Recommended change:** retain mandatory cold validation for setup, toolchain,
Cargo, native build scripts and linker inputs, plus periodic clean assurance.
Use explicitly scoped ordinary source validation. Do not erase the native-vs-Nix
and x64-vs-ARM coverage distinction when narrowing triggers.

### 8. Web always rebuilds the engine but omits real browser execution

Every matching web event regenerates the committed engine, even when only
browser Rust source changed (`web-check.yml:58-65`). The engine's pinned source,
compiler and normalization inputs are independent of ordinary browser edits
(`scripts/build-vt-wasm.sh`). There is no explicit target/Zig cache in this lane.

Yet its outer paths omit root `Cargo.toml`, `.cargo/**`, and the directly-called
`scripts/install-zig.sh`. Browser path dependencies inherit workspace manifest
settings. The browser workspaces have their own lockfiles, so omission of root
`Cargo.lock` is not the same defect as omission of root `Cargo.toml`.

The two test commands use `wasm-pack test --node`. Actual canvas and live-server
tests declare `run_in_browser` (`clients/phux-web/tests/render.rs:10` and
`tests/e2e_browser.rs:15`). No workflow invokes their headless-Chrome/server
setup. "Browser session tests" therefore overstates the exercised environment.

**Recommended change:** gate engine reproduction on its own immutable inputs,
run adapters/session tests for browser changes, include actual shared inputs,
and fund explicit browser-rendering/e2e coverage with the recovered budget.
Native libghostty, WASM libghostty, Node tests and real browser tests remain
different build/runtime boundaries.

### 9. Small exact duplicates should not distract from the structural cost

- Cockpit invokes the CLI build explicitly and through the Zig install/package
  graph. In the sampled logs, subsequent Cargo builds take **0.14-0.40s**.
  They are redundant invocations, not another multi-minute compile.
- Pi's pack and load-smoke npm commands each run the same clean production
  TypeScript build (`integrations/pi/package.json:44,46,50-51`).
  Share the built test artifact while
  retaining both packaging and host-load assertions.
- The first ignored e2e pass uses `--run-ignored all`, repeating some nonignored
  harness/argument tests from the unit pool. This is execution duplication on
  the already-built binaries, not the source of the 313s initial build.
- Monthly Rust mutation source-installs cargo-mutants; the Zig pilot repeats a
  pristine baseline per selected mutation. Cache immutable tool installation
  first; baseline reuse needs a tool-contract experiment. These scans are not
  an ordinary-PR fan-out problem.

## What the evidence does not support deleting

- Linux x64, Linux ARM, and macOS ARM releases serve different machines.
- Clippy/rustdoc and tests do different work, with different features/artifacts.
- Native smoke tests the documented non-Nix setup and a different Linux
  architecture; keep that coverage while narrowing its trigger surface.
- Zig null-platform tests do not substitute for compiling the AppKit app.
- Packaged-app lifecycle, codesigning, ZIP/DMG verification test shipping bytes.
- SDK HEAD deliberately changes the dependency revision. It is not a second
  test of the pinned dependency.
- Scheduled stress and mutation are already separate from ordinary full PR CI.
- Metadata jobs add visible boxes but are not the multi-minute build hotspot.

## Acceptance evidence for a redesigned pipeline

A useful improvement preserves the required check contexts and supported
platforms while reporting fewer build owners and fewer expensive executable
targets. Measure three separate quantities: sum of runner-seconds, critical-path
wall time, and actual billed cost. Record cacheable and non-cacheable requests
alongside hit rate. Compare the same revision/toolchain/features/target/profile;
warm-cache and cold-cache results answer different questions.

Workflow routing needs fixtures for docs, integration-only, Cockpit-only,
browser-only, workflow-only, shared protocol/FFI/Cargo, and mixed changes on
both PR and push events. Artifact reuse needs immutable source/configuration
identity and successful shipping-byte checks. The desired unit is "one build
per necessary input set", not "one enormous job".

## Follow-up ownership

The audit is tracked as `phux-95lk`. Implementation evidence belongs in Beads:
`phux-qyo2` owns routing and cheap-only changes; `phux-m8d5` owns canonical
Cockpit compilation and the FFI/CLI feature-union experiment; `phux-nj0k` owns
web reproduction/coverage boundaries. Existing `phux-mmxz` received current
test-artifact evidence, and `phux-7k11` received main/release pipeline findings.

## Reproduce the measurements

```sh
gh api 'repos/no-phux/phux/actions/runs/34314197101/jobs?per_page=100'
gh api --allow-escape-sequences repos/no-phux/phux/actions/jobs/102346960565/logs
gh api --allow-escape-sequences repos/no-phux/phux/actions/jobs/102344343753/logs
gh api --allow-escape-sequences repos/no-phux/phux/actions/jobs/102344443309/logs
```

Use the exact job IDs to preserve attempt identity. GitHub cache inventory is
not sufficient to diagnose the Blacksmith cache: the API returned no current
`cockpit-zig-`/`v0-rust-cockpit-` keys even though the completed logs prove cache
restores. No claim of current cache eviction or cache-backend billing is made
from that discrepancy.
