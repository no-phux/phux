# Guards: proving a regression test can fail

A regression test that has never been seen failing is a test-shaped comment.
This document describes the ritual that turns one into a **guard**, the two
scripts that support it, and — deliberately — the parts of the problem the
scripts do not solve.

## The failure this exists to prevent

On 2026-08-10 a regression test PASSED against the very bug it was written to
catch. It drove a window resize with four panes open and allowed "a few more
frames" for the layout to settle. The old behaviour it was guarding converged
exactly one pane per frame, so four panes needed exactly four frames — inside
the allowance. The test was green against the bug and green against the fix,
and nothing about reading it revealed that.

It was caught only because someone disabled the fix by hand and re-ran, at
which point the test was tightened to assert EXACTLY ONE viewport dispatch.
That step — remove the fix, watch the test go red, put the fix back — is the
only thing that distinguishes a regression test from a hope. It is now
`scripts/guards/one-frame-convergence.guard`.

## The ritual

1. Write the fix and the test.
2. **Remove the fix in the working tree.** Not something like it — the fix.
3. Run:

   ```sh
   scripts/guard-red-run.sh --record <guard-name> \
       --test "the exact zig test name" <path/you/edited>
   ```

   This captures the break as a patch, restores your tree, runs a green
   baseline, re-applies the break, runs the suite, and requires that **the test
   you named** appears in the failure. Then it restores and records the
   evidence in `scripts/guards/<guard-name>.guard`.
4. Add the marker directly above the test:

   ```zig
   // GUARD: <guard-name>
   test "the exact zig test name" {
   ```
5. Write the prose at the top of the guard file: what the defect was, and what
   removing the fix puts back. That prose is what a reviewer reads.
6. Say so in the commit message.

If step 3 does not go red, you do not have a regression test. That is the
whole point, and it is not a formality — the script has two failure modes it
will report rather than record:

- **NOT A GUARD** — the suite stayed green with the fix removed.
- **WRONG RED** — the suite failed, but not at the named test. A break that
  stops the tree compiling looks exactly like a successful red run if you
  judge by the exit code alone, and proves nothing.

## The two halves, and what each costs

| | `guard-check.sh` | `guard-red-run.sh` |
|---|---|---|
| Runs | every `zig build test` | on demand |
| Costs | milliseconds | ~40s per guard, plus a baseline |
| Proves | the bookkeeping is still true | the test still cannot pass without its fix |

`guard-check.sh` is wired into the gate (`build.zig`, `addGuardCheck`). It
checks that every marker has a guard file, every guard file names a test that
still exists and carries the marker, every guard has been demonstrated red,
and **every recorded break still applies to the tree**.

That last check is the staleness test, and it is the closest a static check
gets to the real question. A guard's `red:` line records a run against a
particular shape of the fix. When the fix moves, the patch stops applying, and
the gate says so — which is a demand to re-derive the guard and watch it go
red again, not a claim that anything is broken.

## What this does NOT prove

Being honest about the gap is the point of writing it down.

- **A guard that still applies is not a guard that still fails.** Code around
  the fix can change so that the break no longer produces the old behaviour
  while the patch still applies cleanly. Only `guard-red-run.sh` answers that,
  and it only answers it the day you run it.
- **Most tests are not guards and should not be.** Guards are for tests that
  claim to pin a specific past defect. A contract test that has never had a
  bug behind it has nothing to be shown red against.
- **The break is a hand-authored counterfactual.** It is supposed to be the
  code as it actually was. Nothing enforces that, and a break invented to make
  a test go red proves only that the test can fail, not that it would have
  caught the bug. Write the break from the real prior behaviour, and say in
  the prose which behaviour that is.
- **The phux provider is usually not in the run.** A default build reports
  `PASS, INCOMPLETE`; a guard whose break lives under `src/providers/phux/`
  cannot be proved by it. None of the current guards do.

## Reading a guard file

```
# GUARD <name>
#
# Prose: the defect, and what the break puts back. Preserved across re-runs.
#
test: <the exact zig test name>
build: <zig build arguments>     <- optional; the graph the test lives in,
                                    e.g. "test -Dplatform=null". Absent means
                                    "test".
red: <date> at <commit>          <- written ONLY by guard-red-run.sh
diff --git ...                   <- the break, as a git patch
```

`red:` is not editable bookkeeping. `guard-red-run.sh` is its only writer and
writes it only after watching the named test fail, only if the suite was green
before, only if the build's own verdict named this worktree as its source root,
and only if the checkout did not move underneath the run.
