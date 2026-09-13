---
audience: contributors
stability: stable
last-reviewed: 2026-08-09
---

# 0082 — Retire the CI metrics store; the run page is the dashboard

**TL;DR.** The orphan `ci-metrics` branch, its collector workflow, and the
weekly `observatory` lane are deleted. CI observability is now exactly what a
run renders into its own step summary. If a hosted dashboard comes back, it
belongs to the site, fed by a source phux does not have to run.

Status: Accepted
Date: 2026-08-09

## Context

[ADR-0047](./0047-ci-metrics-branch.md) bought durable CI observability with a
git branch as the database: a collector workflow fired on every completed run
of six tracked workflows, swept the Actions API, folded in artifact-borne
records, and pushed NDJSON plus a rendered `DASHBOARD.md` and
`site/summary.json` to an orphan `ci-metrics` branch. It worked — 953 bot
commits and ~3.2 MB of records in under a month.

That is also the problem. A high-frequency bot branch sits in every
`git branch -a`, every fetch, and every branch picker, in a repo whose
day-to-day branch list is how work gets navigated. The machinery is
disproportionate to its use: the questions it answers (is `check` getting
slower, what did that bump cost) get asked rarely, and when they are asked,
the run page and a local `cargo clean && just timings` answer them. The
per-run collector, the single-writer concurrency rule, the idempotent sweep,
the fork-artifact trust boundary, and the schema validation are all cost paid
continuously for a lookup that happens occasionally.

## Decision

Delete the store and everything that exists to feed it:

* the `ci-metrics` branch and the `ci-metrics` workflow that was its sole
  writer;
* the `observatory` workflow (weekly cold dev/release `--timings` builds,
  binary size with bloat attribution, dependency stats);
* `scripts/ci/{collect-runs,render-dashboard,parse-timings,binary-size}.sh`
  and the `ci-metrics-*` artifact uploads in `ci.yml` and `stress.yml`;
* `just ci-report`.

Keep the free half: `scripts/ci/timed.sh` still wraps each cargo phase and
`scripts/ci/summarize-job.sh` still renders phase timings, cache hit/miss,
target-dir size, and slowest tests into the run's step summary. Nothing
outlives the run, so `summarize-job.sh` no longer emits a machine-readable
record and the scratch directory is `target/lane-signal`, not
`target/ci-metrics`.

A hosted dashboard is not ruled out — it is moved off this repo. If
<https://phux.phall.io/ci> returns, the site owns the collection and the
storage, and phux stays a repo that builds and tests itself. Nothing in this
decision blocks that; it removes the assumption that the data must first pass
through a phux branch.

## Why

The store's own cost model beat it. Every tracked run paid a collector run,
and the collector's whole design — single writer, sweep-and-diff,
schema-validate untrusted artifact input — exists to make a git branch behave
like an append-only database it was never meant to be. Deleting it removes
one workflow, four scripts, three artifact uploads, two weekly cold ARM
builds, and a branch, and costs a trend line that was consulted about as
often as it was regenerated.

The step summary survives because it is the part with no marginal cost: it
runs inside a lane that was already running, needs no store, no token, and no
second workflow, and it answers the question at the moment someone actually
has it — while looking at the run.

## Tradeoffs

Historical trend is gone, not archived: the ~3.2 MB of NDJSON dies with the
branch, and CI wall times are only observable for as long as GitHub retains
runs. Cold-build timelines, binary-size attribution, and dependency-count
drift are no longer sampled on a schedule, so a slow regression across many
small bumps will be noticed later, and by a human running `just timings`
rather than by a chart. <https://phux.phall.io/ci> loses its data source and
comes down with this change.

## Alternatives

Move the store to the site's edge (R2/KV behind the demo Worker, ingested
from CI): keeps full fidelity and full history, but trades a branch for a
storage binding, an ingest route, and a push secret in this repo — more
machinery than the retired dashboard justified.

Rebuild the rollup from the Actions API at site build time: no store and no
branch, but artifact-borne detail needs a cross-repo token, and it keeps a
scheduled job alive to serve the same rarely-asked question.

Keep the branch and prune it: the branch noise in the branch list is the
complaint, and pruning shards does not fix that.
