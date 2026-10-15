---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-27
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

The permanent deliverable is the behavioral test; there is no `// GUARD:`
marker, saved patch, or patch-applicability check.

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

Run from the repository root. The Rust runner documents its supported
scope and budget options:

```sh
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

### CI

The [scoped-mutation workflow](../.github/workflows/mutation.yml) runs bounded
Rust pilots on manual dispatch.
It is separate from required PR checks. Diagnostic artifacts are retained for
14 days, including on failed runs when the runner produced them. There is no
mutation-score threshold; baseline and tool errors still fail the scan.

## Scope and execution boundaries

Start with a small file or changed-code scope. Bound the number of mutants,
workers, and execution time. A capped run is a sample, not exhaustive coverage;
a diff-scoped run also misses weaknesses elsewhere caused by the change.
Normal Rust checks do not install or execute mutation
tools.

Rust mutation runs exercise the selected Cargo tests, not every native
presentation path. Cross-boundary evidence requires rebuilding and exercising
the affected consumer from the same mutated checkout. An archive built from
unmodified sources invalidates that claim.

The former Cockpit-only Zig mutation pilot is preserved with the
[archived client](./consumers/cockpit.md), not maintained on main.
