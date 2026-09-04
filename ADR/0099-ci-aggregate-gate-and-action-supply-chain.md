---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-04
---

# 0099 — CI: one aggregate merge gate, immutable action pins, and shared lane setup

**TL;DR.** The merge contract is a single `ci` aggregate context whose skip
policy is explicit; every action reference is pinned to a commit SHA and
bumpted by Dependabot; the Linux Rust lane setup exists once as a composite
action; and the cockpit workflows now cache what they previously rebuilt from
scratch every run.

Status: Accepted
Date: 2026-09-04

## Context

Cockpit's import into this repository doubled the CI surface without a
corresponding pass over the CI story, and the Blacksmith runner cutover
(#519) landed as a mechanical `runs-on` sweep. The combined result:

* **Merge contract spread across raw lane contexts.** `check`/`test` were
  the required contexts, but both jobs are gated *off* by the `changes`
  classifier for docs-only and cockpit-only PRs. Branch protection on raw
  contexts therefore depends on GitHub scoring a skipped job as satisfied —
  true today, invisible everywhere, and breakable by any future gate
  condition.
* **No cancellation on PR close.** Merging or closing a PR left its
  queued/running lanes burning Blacksmith minutes on a result that no longer
  mattered.
* **Mutable action tags.** The phux workflows referenced actions by major
  tag (`@v7`, `@v22`), while the cockpit-imported ones pinned SHAs. A
  retagged release could land code into every workflow with no diff in this
  repository.
* **Copied setup had already drifted.** The Nix/Cachix/CPU-fingerprint/
  sccache/rust-cache block was copy-pasted across ci.yml's two lanes and
  stress.yml — and stress had already lost the sccache half to drift.
* **Cockpit rebuilt everything cold.** `cockpit-ci` ran `cargo build` for
  the FFI and the full Zig graphs on every PR with no cache at all; the
  release and SDK-head lanes did the same.
* **Toolchain versions were hardcoded** in `release.yml` and
  `release-please.yml` (`1.90.0`) while `rust-toolchain.toml` pinned the
  channel — a second, decaying source of truth.
* The cutover itself introduced latent breakage: actionlint's Blacksmith
  label list was never extended (so `workflow-check` — and the CI `check`
  lane that runs it — went red on main), and the npm publish lane was
  relabeled off the GitHub-hosted runner npm trusted publishing requires.

## Decision

1. **One aggregate gate.** ci.yml ends in a `ci` job that folds
   `changes`/`check`/`test` into a single verdict: the classifier must
   succeed; compile lanes may be `success` or `skipped` (skipped is the
   fail-safe fast path, by policy); anything else fails. The ruleset should
   require `ci` and leave the raw lanes reported-but-unrequired. `if:
   always()` on the aggregate is load-bearing — a red lane must not skip the
   aggregate into a green-by-skip.
2. **PR janitor.** `pull_request: closed` cancels queued/in-progress runs
   for the PR's head SHA (fork-safe; push runs on `main` are never touched).
3. **Immutable pins, auto-bumped.** Every remote `uses:` is
   `owner/repo@<40-hex> # v<tag>`; `scripts/check-action-pins.sh` (wired
   into `just workflow-check`) enforces the shape and Dependabot (grouped,
   weekly) proposes bumps.
4. **One lane-setup composite.** `.github/actions/setup-rust-lane` owns disk
   headroom, CPU fingerprint, Nix + Cachix, optional sccache, and
   rust-cache. Cache keys are profile-scoped (`check`, `test`) so the stress
   nightlies share main's warm `test` entries instead of maintaining a third
   copy. PR runs restore but never save.
4b. **Workflow-only PRs never compile.** When every changed file is CI
   infrastructure (workflows, composite manifests, actionlint.yaml), ci.yml
   runs a compile-free `workflow-gate` job (actionlint, pin check, truth
   tables, orchestration assertions) and skips the compile lanes; cockpit-ci
   runs the same classifier and skips its macOS job when nothing but
   workflow files changed. Dependabot's grouped action bumps — previously a
   full 25-minute compile each — cost minutes. The classifier is truth-table
   tested (`scripts/ci/check-classify-changes.sh`); unknown paths still fail
   closed into full CI.
5. **Cockpit caches like ci.yml.** FFI builds share a macOS `cockpit-ffi`
   rust-cache key across cockpit-ci, cockpit-release, and cockpit-sdk-head;
   Zig global + project caches are keyed on the checked-out build manifest
   and Zig version, restored every run and saved only from main (on a miss).
6. **Single source of truth for the toolchain.** Workflows derive the Rust
   channel from `rust-toolchain.toml`; hardcoding a release there is
   forbidden in both release paths.
7. **Caches never decide release bytes.** Release binaries are still built
   from scratch — only the cargo registry is cached (`cache-targets:
   false`), because dependency downloads are content-identical while target
   artifacts must not be shared across release tags.

## Consequences

* The `ci` context means exactly one thing, and the skip policy lives next
  to the gates it serves.
* The rust-cache keys change once on adoption (one cold rebuild per lane).
* A tag compromise in any action no longer reaches this repository without
  a Dependabot PR; the pin check fails the build if a future edit floats a
  tag back in.
* If npm ever relaxes trusted publishing to Blacksmith's runners, the
  publish lane may be relabeled — but only after npm confirms OIDC
  acceptance, not before a release cut.

## References

* `scripts/ci/classify-changes.sh` — the fail-closed gate the aggregate
  protects.
* release-drift — the scheduled alarm for releases whose lanes silently
  never finish; this ADR's cancellation and pinning changes do not touch
  that contract.
