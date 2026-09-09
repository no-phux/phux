---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Scoped mutation testing

**TL;DR.** Keep behavioral regression tests in the ordinary test gate. Use
bounded, opt-in mutation runs to find gaps in those tests. Record results as
run artifacts; do not maintain source patches or require a perfect mutation
score.

## Regression policy

For a bug fix, demonstrate that its named regression test fails against the
actual bug and passes with the fix. Record the revision or change removed,
the command, and the observed assertion failure in the commit or PR. A compile
error, missing dependency, or test-runner failure is not a reproduced bug.
Restore the fix and run the relevant ordinary gate before committing.

The permanent deliverable is the behavioral test. There is no required
`// GUARD:` marker, saved `.guard` patch, or build-time patch-applicability check.
The former Cockpit system is described in its
[historical guard guide](../clients/cockpit/docs/GUARDS.md); old acceptance
records remain evidence of the runs performed at those revisions.

Automatic mutation testing asks a complementary question: can a tool insert a
small change into current production code without the tests noticing? Generated
mutations need not reproduce a historical defect. Neither technique replaces
review, ordinary tests, or native interactive acceptance.

## Reading results

| Outcome | Meaning | Response |
|---|---|---|
| Killed / caught | The mutation compiled and tests rejected it | Evidence for this mutation and this test scope |
| Survived / missed | The mutated program passed the selected tests | Inspect the behavior; add a meaningful test if it exposes a gap |
| Unviable / compile error | The mutation could not build | Not evidence that a behavioral test caught it |
| Timeout | The run exhausted its budget | Inspect the log for a hang or insufficient budget; do not count it as an assertion failure |
| Baseline / tool error | Unmodified tests or the runner failed | Fix the run before interpreting mutation results |
| No targets | The selected change contains no supported mutation targets | Explicitly empty scope, not a mutation pass |

Some survivors are behaviorally equivalent, unreachable in the supported
configuration, or outside the selected tests. Do not add tests that merely
repeat the implementation to make a score green. Track substantive uncovered
behavior in the usual issue tracker. Keep raw per-mutant diagnostics with the
summary so compile errors and timeouts cannot disappear into a single score.

## Commands and CI

Run from the repository root. Each language runner documents its supported
scope and budget options:

```sh
just mutation-zig --help
just mutation-rust --help
```

### Rust

Install the runner's exact `cargo-mutants` version into a checkout-local tool
directory (the version pin lives in `scripts/mutation/rust_runner.py`):

```sh
version="$(python3 -c 'import runpy; print(runpy.run_path("scripts/mutation/rust_runner.py")["VERSION"])')"
cargo install cargo-mutants --version "$version" --locked --jobs 2 \
  --root "target/mutation-tools/cargo-mutants-$version"
just mutation-rust
just mutation-rust --file crates/phux-core/src/window.rs --limit 16
just mutation-rust --in-diff origin/main --list
```

An exact-version executable on `PATH` or selected through `CARGO_MUTANTS_BIN`
also works. The runner never installs tools implicitly. Its default pilot
selects at most eight mutations in `phux-core`'s `window.rs`, with one mutation
worker, two compiler tasks, a 300-second build timeout, and a 60-second test
timeout. Each tool invocation has an outer 1,800-second limit. Discovery and
sampling are deterministic; the first slice is not a random or exhaustive
sample. Review `selected.json` before interpreting the result.

`--in-diff REF` compares the merge base of `REF` and `HEAD` with tracked working
tree content, including staged and unstaged edits. Untracked files are not in
Git diffs. Fetch the reference and sufficient history first in a shallow clone.
File, package, regex, and diff filters intersect. Use `--list` to inspect that
intersection without compiling mutants.

Reports go into a unique `target/mutation/rust-*` directory by default, or a
new directory supplied with `--output`. `summary.json` records the scope,
baseline, outcome counts, raw exit status, and commands; `mutants.out` retains
per-mutant logs and diffs. Exit 0 includes completed findings and explicitly
empty scopes, exit 4 means baseline failure, and exit 1 means tool failure or
incomplete results. Cargo mutation scratch copies have private target
directories; registry and Git download caches may be reused.

SIGINT and SIGTERM stop the active invocation, preserve partial logs, and
write an `interrupted` summary with exit 130 or 143. Cleanup allows five seconds
for cargo-mutants to unwind, then escalates against tracked descendant PIDs,
including descendants in separate process groups. This requires the standard
`ps` utility on macOS or Linux.

The adapter's real-tool acceptance check uses disposable fixtures to verify
caught, missed, unviable, timeout, failing-baseline, diff, sample, and cleanup
behavior:

```sh
python3 scripts/mutation/rust_check.py
```

### Zig

Use Python 3.12+, Git, and the repository-pinned Zig. The runner downloads a
Zentinel source archive at an immutable revision, verifies its SHA-256, and
builds and tests the tool with two compiler jobs. The pin lives in
`scripts/mutation/zig-tool.json`. Subsequent runs reuse the verified binary
under `$XDG_CACHE_HOME/phux/mutation/zentinel` (default `~/.cache`). Source
mutations and compiler caches remain in disposable private workspaces;
Zentinel result caching is disabled.

```sh
just mutation-zig --list --out target/mutation/zig-list
just mutation-zig --max-mutants 8 --out target/mutation/zig-pilot
just mutation-zig --diff HEAD --out target/mutation/zig-diff
just mutation-zig-check
```

This is an explicit **standalone-module pilot**: the current allowlist in
`clients/cockpit/mutation/pilot.json` contains Cockpit's real
`src/cockpit/native/ts_protocol.zig`. Its source and built-in tests are copied
verbatim. `--scope` selects an allowlisted repository-relative file; unsupported
files are rejected. Expanding the allowlist requires validating that module's
imports and test command. The pilot does not run the full native app graph.

`--diff REF` selects supported files changed against that exact commit,
including tracked working-tree edits. Unlike Rust's `--in-diff`, this is
**file-scoped**, not changed-line-scoped, and does not compute a merge base.
An irrelevant diff writes `no_targets` without installing Zentinel; an invalid
reference fails. Output directories must be new or empty.

The default is eight mutations, serially, with 60 seconds per baseline/mutant
command and an outer per-invocation bound. The hard count cap is 100. Zentinel
has no native count cap, so the adapter selects generated IDs and invokes
`--mutant` for each. Each invocation repeats the baseline with cold caches;
this cost is why the pilot starts small. `summary.json` records the source
hashes, tool pin, scope, budgets and outcomes. `candidates.json` and unchanged
`mutant-NNN.json` reports plus command logs retain the underlying evidence.
Acceptance checks retain reports, logs and configs under unique
`target/mutation/zig-check-*` directories even when an assertion or class setup
fails; disposable compiler caches and binaries are removed.

Killed, survived, compile-error and timeout outcomes stay distinct. Survivors
do not fail a score gate; a failed baseline or tool invocation fails the run.
Missing, mismatched, skipped, invalid, compiler-crash or inconsistent mutant
reports also fail the scan, even when Zentinel itself exits zero. Raw reports
are retained before validation.
Process cleanup covers the launch group and observed descendants, including
separate groups. It is not kernel isolation: immediate unobserved daemonization
or SIGKILL of the controller can escape cleanup.

The full shipping graph additionally requires same-checkout Rust FFI, Native
SDK/Ghostty, and Node. Zentinel's minimal command environment strips arbitrary
FFI/SDK environment overrides, so this adapter deliberately makes no claim
about that graph. Tool and runner adoption were verified on macOS arm64; a
Linux CI run is separate evidence. At the pinned revision, Zentinel's aggregate
`run.duration_ms` is zero; use individual command durations for timing.

### CI

The [scoped-mutation workflow](../.github/workflows/mutation.yml) runs bounded
pilots **monthly**, at 05:23 UTC on the first day of the month, to limit CI
cost. It also supports manual dispatch with Zig, Rust, or both selected.
It is separate from required PR checks. Diagnostic artifacts are retained for
14 days, including on failed runs when the runner produced them. There is no
mutation-score threshold; baseline and tool errors still fail the scan.

## Scope and execution boundaries

Start with a small file or changed-code scope. Bound the number of mutants,
workers, and execution time. A capped run is a sample, not exhaustive coverage;
a diff-scoped run also misses weaknesses elsewhere caused by the change.
Normal `just cockpit-test` and Rust checks do not install or execute mutation
tools.

Rust mutation runs exercise the selected Cargo tests. A Zig module pilot
exercises the selected module's tests. Neither implies that the full
Rust → C ABI → Zig → native presentation path was tested.

To claim cross-boundary evidence, rebuild `phux-client-ffi` from the mutated
checkout and link that archive into the Cockpit test graph from the same
checkout. Run via Cockpit's private-cache build wrapper and check the source
root, exit status, and complete Phux verdict. An existing archive from the
unmodified tree invalidates that claim. Never edit source while Zig is building
it. See [Cockpit setup](SETUP.md#cockpit) for the ordinary shipping graph gates.

## Adoption evidence

During the September 2026 guard retirement, a byte-level inventory confirmed
that all 542 existing Cockpit test declarations and their bodies were retained;
only 115 marker comments changed in source code. The same-checkout shipping
test gate passed both before and after retirement (535 passed, 2 skipped),
and the app build passed after retirement.

The Rust default pilot passed its 50-test unmodified baseline and caught all
eight selected mutations. Disposable fixtures independently demonstrated one
caught mutation, one survivor, one compile failure, and one timeout, plus a
failing baseline that prevented mutation execution. These are bounded adoption
checks, not a claim of comprehensive mutation coverage.

A follow-up scan of the layout rejection paths found five survivors among
seven mutations. Package-local tests now verify that invalid split ratios,
missing split targets, and missing kill targets return the correct error and
preserve the existing layout. The same seven mutations were then all caught,
with the unmodified package baseline passing. This strengthened public-contract
coverage; production layout code was unchanged.

The Zig pilot initially killed seven of eight selected mutations. The survivor
changed the intent decoder's length/version `or` to `and`. A new module-local
test rejects unsupported versions, every shorter packet length, and an overlong
packet. The identical generated mutation then failed that named test at the
version assertion; all eight selected mutations were killed with all six
baseline tests passing. The final shipping Cockpit gate passed 536 tests with
two skipped, and the app build passed. Production decoder code was unchanged.
