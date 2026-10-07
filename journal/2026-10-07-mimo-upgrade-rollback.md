---
audience: agents
stability: scratch
last-reviewed: 2026-10-07
---
# Upgrade rollback after a failed handoff

Mimo's awaited unseal fixes the preparation-error race but can wait forever
for an actor that already missed its cut deadline. Its serial traversal also
leaves responsive neighbors sealed behind that actor. Exec failure still uses
only the older asynchronous Drop fallback.

The rollback contract is to attempt all captured sessions concurrently, wait
for responsive actors to acknowledge unseal within one aggregate two-second
window, and then report the original upgrade error. Closed actors owe no
rollback. A timed-out or cancelled rollback retains unresolved senders in its
guard, so Drop still attempts their unseal without blocking the caller. An
acknowledged sender is removed immediately, including when a later rollback
is cancelled. No failed append consumes sequence space.

Use the same awaited rollback after a failed exec attempt. Restore descriptor
flags immediately after exec returns, before awaiting actors; this avoids
extending the inheritable-descriptor window while rollback runs. Successful
exec still replaces the process directly.

Validation uses paused time and explicit future polling for full mailboxes,
lost actors, missing acknowledgements, cancellation and neighbor independence.
Real preparation and exec-failure fixtures use owned temporary files and
in-memory resource actors. No installed executable, production socket or
production state is touched. The parent owns Beads issue phux-pfd and release
integration; this worktree owns the rollback code and its scoped commit.

The review also found a separate, existing ownership issue: Upgrade is
dispatched per connection, while the actor's seal is one boolean. Two
simultaneous preparations can overlap, and rollback from one can clear the
other's seal before its exec. The parent will track single-upgrade admission
or seal ownership separately; this change does not expand admission semantics.

The first focused nextest run built successfully with Rust 1.99.0 and Zig
0.16.0 through mbx 1.22.0. It demonstrated both liveness failures in Mimo's
unfinished implementation: the healthy mailbox received no unseal while its
neighbor was full, and an actor that accepted but retained its reply kept lift
pending after advancing the paused clock by two seconds. Two additional
assertions inspected the sender-vector representation rather than observable
rollback behavior; these were removed, since the older spent boolean also
correctly suppressed redundant Drop work in those successful cases. The final
tests assert mailbox delivery, acknowledgement barriers and absence of repeated
unseals instead. Red evidence: /tmp/phux-upgrade-rollback-red.log.

Implemented concurrent bounded lift and removal of each acknowledged or closed
sender. Preparation errors await it, and failed exec now restores descriptor
flags before awaiting the same rollback. Added a cancellation-progress test
and an actual failing exec syscall against an owned non-executable tempfile,
including first-append sequence continuity.

Focused validation completed:

- `mbx +1.99.0 nextest run --locked -p phux-server --lib -E
  'test(runtime::upgrade::tests::) |
  test(resource::agent_session::tests::a_queued_unseal_)' --no-fail-fast`:
  all 27 selected tests passed, 1,197 other tests skipped. The two liveness
  regressions are green. Log: /tmp/phux-upgrade-rollback-green.log.
- `mbx +1.99.0 clippy --locked -p phux-server --lib -- -D warnings`:
  passed. Clippy first identified accesses to the old underscore-prefixed guard
  fields; renamed these fields and formatted their constructors, without
  changing ownership or behavior. Final log:
  /tmp/phux-upgrade-rollback-clippy-final.log.
- Rustfmt and Git whitespace checks passed.
- Final independent review found no blocker. Its conditional test-comment
  correction was applied. Report:
  papers/2026-10-07-upgrade-rollback-review.md.

This is scoped native coverage, not a whole-workspace or full CI claim. The
parent will integrate this commit and run final server/lifecycle gates and
all-target denied-warning Clippy against the combined changes. No remote push
or production operation was performed by this worker.
