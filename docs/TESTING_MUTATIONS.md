---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-17
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
The permanent deliverable is the behavioral test; there is no `// GUARD:`
marker, saved patch, or patch-applicability check.

Mutation testing asks a complementary question: can a tool insert a small
change into current production code without the tests noticing? Neither
technique replaces review, ordinary tests, or native interactive acceptance.

## Reading results

| Outcome | Meaning | Response |
|---|---|---|
| Caught | The mutation compiled and tests rejected it | Evidence for this mutation and this test scope |
| Missed | The mutated program passed the selected tests | Inspect the behavior; add a meaningful test if it exposes a gap |
| Unviable | The mutation could not build | Not evidence that a behavioral test caught it |
| Timeout | The run exhausted its budget | Inspect the log for a hang or insufficient budget |
| Baseline / tool error | Unmodified tests or the runner failed | Fix the run before interpreting mutation results |
| Empty scope | The selection contains no mutation targets | Not a mutation pass |

Some survivors are equivalent, unreachable, or outside the selected tests. Do
not add tests that merely repeat the implementation; track substantive gaps in
the usual issue tracker.

## Running it

From the repository root (`just mutation-rust --help` lists every option):

```sh
just mutation-rust-install   # cargo-mutants pin, under target/mutation-tools
just mutation-rust
just mutation-rust --file crates/phux-core/src/window.rs --limit 16
just mutation-rust --in-diff origin/main --list
```

`CARGO_MUTANTS_BIN` or an exact-version `cargo-mutants` on `PATH` also works;
nothing is installed implicitly. The pin lives in `tools/src/mutation/runner.ts`.

- **Scope.** The default is `phux-core`'s `window.rs`. `--file` (glob),
  `--package`, `--re` and `--in-diff REF` intersect. `--in-diff` compares the
  merge base of `REF` and `HEAD` with tracked working-tree text, staged and
  unstaged; untracked files are not in Git diffs, and shallow clones need the
  reference fetched first.
- **Budget.** At most `--limit` (8) mutants: the first deterministic slice,
  not a random or exhaustive sample. `--jobs` 1, build timeout 300s
  (`--build-timeout`), test timeout 60s (`--test-timeout`), and 1,800s per
  tool invocation (`--run-timeout`). `--list` writes the selection without
  compiling; review `selected.json` before interpreting a result.
- **Reports.** A new `target/mutation/rust-*` directory, or a new `--output`
  directory. `summary.json` records status, counts, baseline, the raw tool
  exit, selection sizes and commands; `mutants.out` keeps per-mutant logs and
  diffs. Scratch copies get private target directories.
- **Exit codes.** 0 completed findings or empty scope, 1 tool error or
  incomplete results, 2 usage error, 4 baseline failure, 130/143 for
  SIGINT/SIGTERM (status `interrupted`, partial logs kept). Shutdown allows
  five seconds for cargo-mutants to unwind, then kills tracked descendants
  across process groups (needs `ps`).

No workflow runs the pilot: run `just mutation-rust` locally. There is no
score threshold; baseline and tool errors fail it.

## Boundaries

A capped or diff-scoped run is a sample, not exhaustive coverage. Rust runs
exercise the selected Cargo tests, not Rust-to-FFI-to-Zig paths: that evidence
needs the affected consumer rebuilt and exercised from the same mutated
checkout. The former Cockpit Zig mutation pilot is preserved with the
[archived client](./consumers/cockpit.md), not maintained on main.
