---
name: flywheel-review
description: Review a diff through three independent lenses — correctness, security, and simplicity — merge the findings, and record every one in the review ledger. Use at pre-push, before opening a PR, or when asked to review changes in a flywheel repo. Advisory only; it never blocks and never merges.
---

# flywheel-review

One reviewer has one set of blind spots. Three reviewers with *different briefs*
have three smaller ones, and the overlap is where the real defects live.

Replaces the single `ponytail-review` pre-push call. Still advisory — the human
is the merge authority — and still bound by the pre-push time budget, because a
slow hook is a skipped hook.

## Run the lenses in parallel

Give each lens the diff (`origin/main...HEAD` by default) and **nothing else**.
Do not let them see each other's findings: independence is the whole point, and
a lens that reads another's output will agree with it.

**Correctness.** Does this do what the bead said it would? What input makes it
wrong? Concurrency, error paths, boundary values, partial failure. Check the
acceptance criteria on the bead are actually met, not merely addressed.

**Security.** Input validation, injection, secrets in code or logs, authz
assumptions, dependency surface, anything that widens what an attacker can
reach. In a flywheel repo, add: could an unattended agent be induced to do
something ADR 0003 forbids?

**Simplicity (ponytail).** Is there a shorter thing that works? Speculative
generality, duplicated logic, a dependency where stdlib would do, abstraction
with one caller. Also the reverse: cleverness that will not survive being read
in six months.

## Merge, deduplicate, rank

Two lenses reporting the same line is a *stronger* signal, not a duplicate to
discard — collapse them into one finding and say both lenses flagged it.

Rank by what would actually go wrong. Drop anything you cannot state as a
concrete failure: "this could be cleaner" is not a finding, "this loop is O(n²)
over a list that grows with every release" is.

## Record every finding — by running the command, not showing it

**This is a step you execute, not an example you display.** On the panel's first
genuine run it printed this command inside a code fence instead of running it;
the ledger stayed empty, and the hook reported "ran, no findings" for a review
that had just found a real bug. Describing the recording is not recording.

For **each** finding, invoke the Bash tool:

    tools/flywheel/guard.sh finding lens=<lens> file=<path> line=<n> \
      severity=<low|medium|high> claim="<one sentence>" disposition=<accepted|rejected|ignored>

`disposition` is `accepted` (you fixed it), `rejected` (you disagreed — say why
in the claim), or `ignored` (deferred). Record **rejected** findings too: a
ledger of only the good calls cannot produce a false-positive rate, and that
number decides whether the panel is worth its cost.

**If you found nothing, still record that**, so a clean review is
distinguishable from a review that failed to write:

    tools/flywheel/guard.sh finding lens=panel severity=none \
      claim="no findings" disposition=none

## Verify you actually recorded

Before reporting, run:

    wc -l .flywheel/review.jsonl

It must have grown by the number of findings you recorded, plus the `none` line
if you found nothing. **If it did not grow, you did not record** — go back and
run the commands. An unwritten ledger is the failure this panel exists to
prevent, and it has already happened once.

## Stay inside the budget

Three lenses run concurrently should not cost much more wall-clock than one. If
the hook gets slow enough that anyone is tempted to skip it, cut to two lenses
rather than letting it be bypassed — a bypassed gate is worse than a smaller one
(ADR 0001's standing rule).

## Report

Findings ranked, each with file:line, the lens, and a concrete failure. Then one
line: how many findings, how many lenses agreed, and whether anything is severe
enough that you would not merge it yourself.

Never block the push. Never merge. Never edit the audit log.
