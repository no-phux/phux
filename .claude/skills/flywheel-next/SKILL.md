---
name: flywheel-next
description: Take the next ready bead and carry it to a reviewable PR — claim it, reserve territory, build it in an isolated worktree, run the gate, open the PR, and report. Use when asked to "work the next bead", "drain the queue", or when running unattended on a schedule. This is the unit of autonomous work in the flywheel; it never merges.
---

# flywheel-next

One bead, one worktree, one PR, then stop. This is deliberately the *smallest*
useful unit of autonomy — the value is a reviewable diff waiting in the morning,
not an agent that ships product.

**You are bound by ADR 0003.** You MAY branch, commit, run gates, open PRs,
comment on beads, file beads, and reserve territory. You MAY NOT merge, tag,
release, deploy, touch production hosts, read or rotate secrets, force-push, or
rewrite the audit log. If you find yourself wanting to do one of those, escalate
instead.

## 0. Guard — before anything

```bash
tools/flywheel/guard.sh check || exit 0
```

A non-zero exit means the kill switch is set. Exit silently and immediately; do
not investigate, do not "just finish this one thing".

Set `FLYWHEEL_AGENT` to your role name (`<repo>/builder`) so every log line is
attributable.

## 1. Identity

Register with blackbird before acting (ADR 0004):

- `blackbird_agent_register` with `project_key` = the repo's absolute path and
  `agent_name` = `<repo>/builder`.
- Reuse the stored `registration_token` if one exists so you resume the same
  identity across restarts. It authenticates every later call as `agent_token`.
- Never write that token into the repo or a worktree.

## 2. Claim the work

```bash
bd ready --json
```

Take the highest-priority unblocked bead you are competent to do. Skip any bead
whose acceptance criteria you cannot verify yourself — leave it and comment why.

```bash
bd update <id> --claim
tools/flywheel/guard.sh log bead.claimed bead=<id>
```

The claim is atomic but not alive: heartbeat by touching the bead (`bd note`) at
natural checkpoints so a crash is visible as staleness rather than as a bead
that is claimed forever.

## 3. Read the intent before writing anything

`SPEC.md`, the bead's own description and acceptance criteria, `CLAUDE.md`, and
any ADR that governs the area. The bead is the contract; the acceptance criteria
are the definition of done. If the bead is ambiguous enough that two reasonable
implementations differ materially, **escalate rather than guess** (step 8).

## 4. Reserve territory — fail closed

Decide which files you will touch, then reserve the *narrowest* selectors that
cover them, before the first edit:

- `blackbird_reservation_acquire`, `mode: exclusive`, `kind: exact` for single
  files and `subtree` for directories, with a TTL matching your expected run.
- `LEASE_CONFLICT` means another agent owns that ground. Do **not** wait, retry
  in a loop, or edit anyway: release what you hold, comment on the bead naming
  the conflict, unclaim it, and pick a different bead.
- Renew before expiry if the work runs long. An expired lease means you no
  longer hold the ground — stop editing and re-acquire.

No reservation, no edit. If blackbird is unreachable, that is a hard stop, not a
reason to proceed unprotected.

## 5. Build it in a worktree

```bash
git worktree add ../<repo>-<bead-id> -b bead/<bead-id>
```

Worktrees isolate your edits; reservations isolate the eventual merge. You need
both. Work under the repo's standing conventions — ponytail discipline, stdlib
first, test-first, no new dependency without an ADR, `// ponytail:` comments on
deliberate shortcuts.

## 6. Run the gate

The full local gate, exactly as a human would: pre-commit hooks (fmt, vet, short
tests), then the complete suite, then the pre-push review.

**Never bypass a failing gate.** `--no-verify` is not available to you. A red
gate you cannot make green is a step-8 outcome, not a step-7 one.

## 6a. Review your own work before the PR exists (ADR 0013)

Run `/flywheel-review` on your diff. This is where AI review lives now — not in
a git hook. It takes minutes, which is affordable here because you are already
a running agent session and nobody is watching a terminal block; on `pre-push`
the same task got bypassed six times in one afternoon.

Record every finding in the ledger, including the ones you reject, and act on
what you accept before opening the PR. A finding you neither fixed nor recorded
did not happen.

## 6b. Shape the PR before you open it (ADR 0009)

Review capacity is the fleet's binding constraint, and it is spent by
*judgement*, not by line count. Two rules:

**One PR is one idea that can stand alone and be reverted alone.** Group changes
that only make sense together. Never bundle unrelated fixes to save a PR, and
never split one coherent idea to look smaller — both make review harder, in
opposite directions.

**Stack dependent work; do not merge it or split it.** If your bead's change
builds on another that is still in review, branch from *that* branch and open
the PR against it. Each PR then stays small and is reviewed against its parent,
and the stack merges bottom-up. Say in the PR body which branch it stacks on.

Two traps, both hit for real: merging the parent **deletes its branch and
silently closes the child PR** — retarget the child to `main` *before* merging
the parent. And a child branched from the parent still contains the parent's
commits, so after the parent merges, rebuild the child with a cherry-pick onto
`main` rather than a rebase that tries to replay work already upstream.

Declare the review weight on the bead if it is not already labelled: `w:1`
mechanical, `w:2` ordinary, `w:3` a new subsystem or something hard to reverse.
The coordinator budgets in weight, so an unweighted bead costs 2 by default.

## 7. Open the PR

Commit in small imperative commits referencing the bead (`<bead-id>: add retry
backoff`). Push the branch and open a PR whose body states what the bead asked
for, what you did, how the acceptance criteria were met, and anything you
deliberately left out.

Then close out cleanly:

```bash
bd note <id> "PR #<n> opened: <one line>"
tools/flywheel/guard.sh log bead.pr_opened bead=<id> pr=<n>
```

Release every reservation and remove the worktree. **Do not merge the PR** — the
human is the merge authority. Leave the bead open until the PR merges; a PR is
not a closed bead.

## 8. When you cannot finish

The most important path, and the one that must never be papered over. Blocked,
over budget, out of wall-clock, genuinely uncertain, or the gate will not go
green:

1. Leave the bead **open**, with a comment saying exactly where you stopped and
   what you tried.
2. Release every reservation and remove the worktree.
3. `tools/flywheel/guard.sh log bead.abandoned bead=<id> reason=<short>`
4. Escalate to the human via a blackbird message (`escalate`, ack required).

Closing nothing is a correct outcome. Deleting a failing test to make the gate
green, or opening a PR you know is wrong, is not.

## 9. Report

One short summary: bead taken, what you did, gate result, PR link, cost, and
anything you abandoned and why. Under thirty seconds to read.
