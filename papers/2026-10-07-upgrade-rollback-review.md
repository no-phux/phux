---
audience: agents
stability: scratch
last-reviewed: 2026-10-07
---
# Independent review of upgrade rollback

The independent reviewer examined the final rollback implementation and tests
read-only, without compiling or touching Beads, installed binaries, server
sockets or production state. This records its findings; runtime verification
is recorded separately in the journal.

No blocking finding in the patch:

- Send and acknowledgement waits run concurrently within one aggregate
  deadline. Full mailboxes and unanswered requests cannot block healthy
  neighbors.
- Completed senders are removed before the next suspension. Cancellation
  preserves Drop fallback for unresolved actors.
- Failed exec restores descriptor flags before awaiting rollback; descriptor
  owners remain alive during restoration.
- The command caller awaits recovery.
- Tests cover distinct failure classes with direct assertions.

The reviewer requested one wording correction: a test comment claimed that
lift returns only after every actor answers. That guarantee now explicitly
applies to the responsive actor that acknowledged within the deadline.

The simultaneous-upgrade boolean-seal ownership issue is separate and
preexisting. The parent will track admission or owned seals as follow-up work.

Reviewer: /root/review_phux_upgrade/review_upgrade_final. Subsequent cleanup
renamed the two guard fields now accessed by exec from underscore-prefixed
names; their ownership, ordering and behavior are unchanged.
