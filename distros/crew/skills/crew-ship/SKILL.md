---
name: crew-ship
description: Ships a scoped change through native phux missions, isolated worktrees, independent review and authorized publication. Use when the Crew lead dispatches implementation work.
metadata:
  internal: true
---

# Ship with Crew

## Brief before launch

Resolve the repo's absolute path and read its agent/contributor instructions.
Record the objective in the project's tracker if it requires one. A brief:

```text
Role: builder (or reviewer)
Lead home: absolute coordinator home (ownership on restart)
Repository: absolute path
Outcome: observable requested behavior
Scope: files/contracts owned; non-goals
Acceptance: concrete tests/runtime evidence
Authority: local changes / commit / push / PR / merge, exactly as authorized
Handoff: commit, checks, risks, and durable report destination
```

For substantial work, the lead reads the load-bearing code and decides the
seam before assigning the writer. Workers inherit project instructions, not
permission to expand scope or overwrite other people's work.

## Native dispatch

Run in the project checkout, not the Crew home:

```sh
phux mission add 'Fix login redirect' \
  --description 'Role: builder. Outcome: ... Scope: ... Acceptance: ... Authority: ... Handoff: ...' \
  --start --agent opencode --operation-id HEX32 --json
```

Generate a fresh unique nonzero 32-hex ID for each logical add. Record it
before submission and keep the returned alias, revision, branch, worktree,
session and terminal binding. Do not start the same mission twice. A queued
receipt is not a running worker. On uncertain success, reconcile the original
operation; do not create another mission with a new ID.

Existing task: `phux mission show REPO/N --json`, then `phux mission start
REPO/N --expected-revision N --agent KIND --operation-id HEX32 --json`.
Re-read on revision conflict; don't mechanically retry a stale edit.

## Implementation and review

1. Worker traces callers/siblings, implements the smallest complete change,
   and runs focused then proportionate broader checks.
2. Worker reports exact checks, commit, risks and publication status through
   native mail or the durable report destination named in the brief.
3. Lead reads the actual diff and evidence. For substantive changes, send a
   fresh reviewer the base/head, contract and checks. Reviewer is read-only.
4. Disposition every finding. Fix, rerun, review the final diff, then follow
   the authorized publication/review gates. PR creation is not merge authority.

Don't confuse `done_unchecked` or a happy agent message with accepted work.
Never claim full-suite coverage from a scoped check. Do not automatically
delete worktrees, stop workers or merge branches after a report arrives.
