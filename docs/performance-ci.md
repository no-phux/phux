---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-13
---

# Performance CI

**TL;DR.** The required perf-icount job counts instructions on a fixed set of
hot paths, one organization commit at automation scale among them, and fails
when a count rises more than five percent. A 16 MiB flood test bounds server
counters. An org-store scale test bounds pages, batches, writes, and receipts. Nightly wall-time scripts upload artifacts and do
not gate merges. Update a baseline only with just perf-icount --update plus
the Perf-Baseline: update trailer or the perf-baseline label.

## Instruction counts

`perf-icount` in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) is a
required lane of the `ci` aggregate. The ruleset still checks the `ci` and
`commitlint` contexts. The lane runs on `ubuntu-24.04` (x86_64) when the
classifier marks `perf_icount_needed`. Docs-only diffs and crates outside the
hot path skip it. An empty diff runs it.

The job builds `phux-server`'s `icount` bench with the `icount` Cargo profile:
release optimizations, no fat LTO, 16 codegen units. Callgrind then counts
instructions inside one function per scenario, so process startup and runner
load are outside the number. The scenarios are:

- VT apply, a libghostty parse of a mixed text and SGR stream
- frame encode and frame decode of one `RESOURCE_OUTPUT`
- agent-stream append and ring replay
- client-kernel history slice and resync, with a stand-in engine
- the PTY batch copy one actor burst performs
- one organization commit (`org_commit/commit`): a `ResourceMove` through
  `ServerState::commit` into a real SQLite store seeded at the scale contract
  (5,000 resources in 200 sessions, 500 in the Inbox, receipts at the
  10,000 cap), so the count covers apply, the store transaction with its
  receipt pruning, the in-memory mirror, and journaling. Syscalls, including
  the `fsync`, are outside the count. The pinned baseline is about 278,000
  instructions

`just perf-icount` is the same command locally. Run it inside `nix develop`
on x86_64. The script takes valgrind from the flake's nixpkgs so the tool's
libc matches the devshell that built the bench, and it installs the
`iai-callgrind-runner` binary at the version locked next to the
`iai-callgrind` crate. A count from another libc,
another architecture, or the `bench` profile (fat LTO) is not comparable to
the checked-in file. A rustc or flake bump that changes codegen moves the
counts; the path filter runs the job on those files, and the baseline has to
be updated in the same change.

The comparison reads `benchmarks/icount-baseline.json`. Each scenario's
instruction count may rise by at most 5 percent. An equal rise passes. A drop
passes. A missing or extra scenario fails. Exact allocation counts for encode
and decode stay in `crates/phux-protocol/tests/hot_path_alloc.rs` and run with
the unit tests. The instruction job does not compile that test again.

## How to read a failure

The job prints one line per scenario:

```text
ok   vt_apply/apply: measured 100000 baseline 100000 (+0.00%, limit +5%)
FAIL frame_encode/encode: measured 120000 baseline 100000 (+20.00%, limit +5%)
```

`measured` is this run. `baseline` is the committed file. The percentage is
the change from the baseline. `FAIL` means the rise is over 5 percent, or the
name is missing from one side. The `perf-icount` artifact is the raw
callgrind summary (`target/perf-icount/iai.jsonl`) for that run.

A red line on a code change is a regression to explain or a baseline update
when the new cost is intentional. A red line after a toolchain bump, with no
hot-path edit, is the compiler moving the count: update the baseline on
x86_64 under nix and show the toolchain change in the commit.

## How to update the baseline

On an x86_64 machine, inside `nix develop`:

```bash
just perf-icount --update
```

That rewrites `benchmarks/icount-baseline.json` from the run you just
measured. Commit the file with the code that justifies it. On a pull request,
the job still compares the committed numbers to a fresh measurement, and it
also refuses the file change unless one of these is true:

- Some commit on the branch contains a line that is exactly
  `Perf-Baseline: update`.
- The pull request has the `perf-baseline` label before the workflow runs.
  Add the label, then push. Labeling alone does not start `ci`, and a re-run
  keeps the event payload from before the label existed.

Pushes to `main` and merge-queue runs do not repeat the trailer check. Squash
and the merge queue can drop trailers; the pull request already authorized
the file. The percentage check still runs, so a number that does not match
the measurement fails even with the trailer.

## Counter tripwires

`flood_16mib_stays_under_counter_ceilings` in
`crates/phux-server/tests/metadata/flood_counters.rs` writes 16 MiB through a
pane and reads `GET_PERF`. It bounds main-loop handoffs per MiB actually
read (`pty.burst.bytes` samples over `pty.read.bytes`), plus `pump.gap_resync`,
`pump.lagged`, and `pty.reader.blocked`. The ceilings are counts, not time,
and they sit above a quiet run of current main so a loaded runner does not
flake. The test runs in the existing server integration lane.

## Organization store scale

`the_org_store_holds_the_scale_contract` in
`crates/phux-server/src/runtime/org_scale_tests.rs` seeds the same shape as
the `org_commit` scenario ([`org_scale`](../crates/phux-server/src/org_scale.rs))
and runs with the unit tests. It pages the whole inventory and the Inbox
intake, makes 200 single moves, a 256-resource `ResourceMoveMany` and its
undo, then 1,500 renames past the receipt cap, with three event observers
attached. It asserts:

- every `GET_ORGANIZATION` page carries at most `page_size` session and
  resource records together, and the pages hold every record exactly once;
- a batch over 256 resources is refused;
- a single-record commit appends at most 32 WAL pages and the batch at most
  128 (measured 19 and 63; a full rewrite is thousands);
- receipts stay at exactly 10,000 in memory and in the store;
- every observer receives every journaled event, in order.

It prints p50 and p99 commit latency for triage and does not gate on wall
time; the instruction count is the gate.

## Nightly wall time

[`.github/workflows/perf-nightly.yml`](../.github/workflows/perf-nightly.yml)
runs on a schedule and on `workflow_dispatch`. It is not part of `ci.yml` and
it is not a required check. `scripts/bench/perf-nightly.sh` runs a short
attach, echo, and throughput sample (`scripts/bench/mux-compare.sh`), a quiet
echo (`scripts/bench/tui-load.sh`), and session-create timings at 1, 10, and
50 sessions. The run fails when a harness crashes. It uploads `target/perf-nightly`
and does not compare the timings to a budget.

Published host comparisons on the performance page are observations, not
these budgets.
