---
audience: contributors
stability: stable
last-reviewed: 2026-08-09
---

# 0047 — CI metrics recorded to an orphan `ci-metrics` branch

**TL;DR.** CI observability data (per-run job/step wall times, in-lane cargo
phase timings, cold-build timelines, binary sizes, dependency stats) was
recorded as NDJSON on an orphan `ci-metrics` branch by a single collector
workflow, which also rendered `DASHBOARD.md` and a compact `site/summary.json`
that phux.phall.io/ci read directly. Retired by
[ADR-0082](./0082-retire-the-ci-metrics-store.md); the branch, the collector,
and the `observatory` lane are gone.

Status: Superseded by ADR-0082
Date: 2026-07-14

## Context

CI was well-tuned but memoryless: every optimization argument (the two-job
layout, the CPU-keyed cache, the docs-only gate) was made from one-off
observations. Workflow logs expire, artifacts expire in 90 days, and nothing
answered "is `check` getting slower month over month" or "what did that
dependency bump cost cold builds" without archaeology. We wanted durable,
queryable signal with zero new PR-path cost and no external services.

## Decision

Three layers, one store:

* Lanes stay fast and self-describing: `scripts/ci/timed.sh` wraps each
  cargo phase, and a `lane signal` step renders phase timings, cache hits,
  and slowest tests into the step summary, uploading the same facts as a
  `ci-metrics-*` artifact.
* The `observatory` workflow (weekly or manual dispatch)
  measures what PRs must not pay for: cold `--timings` builds for dev and
  release, binary size + bloat attribution, dependency-graph stats.
* The `ci-metrics` workflow is the SINGLE WRITER of the orphan `ci-metrics`
  branch. On every tracked-workflow completion it sweeps recent completed
  runs via the Actions API, appends unrecorded runs plus their metrics
  artifacts to `runs/<YYYY-MM>.ndjson`, and re-renders `DASHBOARD.md` and
  `site/summary.json` (fetched live by the phux-site `/ci` page).

The sweep is idempotent (keyed on run id + attempt), so cancelled or failed
collector runs are simply caught up later; the weekly sweep is the backstop.
Observatory originally also ran on main pushes touching lockfiles or the
toolchain. That made a release-only `Cargo.lock` workspace-version refresh pay
for two cold builds even though required PR CI had already validated the exact
tree. Weekly samples preserve the trend; manual dispatch covers investigations.

## Why

A git branch in the same repo is the only store that is free, permanent,
versioned, diffable, works offline (`just ci-report`), and needs no secrets
beyond `GITHUB_TOKEN`. NDJSON appends never conflict structurally; monthly
shards keep files bounded and prunable. The single-writer rule plus the
idempotent sweep removes the classic concurrent-push failure mode instead of
retry-papering over it. Deriving the dashboard and the site JSON from the
store (never edited in place) means renderers can be rewritten freely.

## Tradeoffs

The branch grows without bound (small: ~2-5 KB per run; old shards can be
deleted, the dashboard only reads 30 days). Artifact-borne records from fork
PRs are untrusted input — the collector schema-validates every line and
caps sizes, and renderers treat records as data, but the dashboard displays
strings from them. Step-level API timings are coarse (seconds) and GitHub
may change payload shapes. The site page depends on raw.githubusercontent
serving the branch (CORS-open today).

## Alternatives

Artifacts only: already had them (build-timings); they expire, and trends
require downloading N zips — that is the archaeology this replaces.

External telemetry (Datadog, Honeycomb, benchmark-action + gh-pages): richer
graphs, but adds accounts, secrets, and a service dependency for a repo that
is deliberately self-contained; benchmark-action also models benchmarks, not
run/step/cache observability.

Committing metrics to `main`: pollutes history and CI triggers; linear-
history rules make bot pushes to `main` actively hostile.
