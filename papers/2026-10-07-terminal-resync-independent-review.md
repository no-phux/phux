---
audience: agents, contributors
stability: scratch
last-reviewed: 2026-10-07
---
# Independent terminal resync review

Reviewed the unfinished Mimo change, L1 sections 4.1 and 4.6, terminal actor
EOF/retention behavior, output task ownership, natural close, cascade and
detach paths. This was a source review: this reviewer ran no compiler, tests,
installed binary, production socket or production-state operation.

The initial bounded gap reservation had an exit ordering defect. A pump parked
on an earlier gap snapshot could miss the actor's single scheduling yield;
RESOURCE_CLOSED could reserve mailbox capacity before the final snapshot.
Oversized batches also used sequential publication and could be split by close.
The existing one-slot two-pump test always takes that oversized path, so it
does not establish that the new fitting-batch wait works.

The revised design addresses the ordering defect with an explicit natural-close
drain. Each terminal pump captures a fresh final checkpoint from the actor,
publishes it, then resolves its completion fence. Close waits for the relevant
client's terminal fences before sending its lifecycle frame. The actor remains
alive until this fanout finishes. Fresh capture avoids a remembered Exit flag
becoming stale across last-shell replacement or unusable at retained expiry.
It also avoids relying on an Exit broadcast surviving the output ring.

The fitting-batch wait reserves every slot before publishing. Timeout or
cancellation releases partial reservations without publishing a prefix.
Oversized publication remains sequential and is not timed out and retried after
a prefix has reached the mailbox. Its natural close ordering instead follows
the completion fence.

Two additional findings were corrected during review:

- Reap initially discarded task tracking before final publication finished.
  Draining tasks now retain abort ownership; the retired wire-to-core mapping
  lets DETACH_RESOURCE stop and await them before its idempotent success reply.
  Client-wide detach also retains access to these tasks.
- Final native publication initially converted PaneGone into a connection
  fault. The native helper's error now propagates, and dropped/unsent synthesized
  capture likewise means PaneGone. Capture timeout/refusal remains a fatal
  publication failure and is not reported as successful final delivery.

Agent tasks do not participate in terminal drain fences. Live cascaded child
tasks are aborted rather than asked to wait for an Exit they may never emit.
Committed kills retain their immediate task cancellation behavior. Releasing
ordinary tasks now drops their tracking values without the former ManuallyDrop
leak. Final-drain cleanup removes the pending wire mapping and tracking.

No further concrete production-source blocker was found in the revision
reviewed. This is not a test-pass or release-readiness claim. Required executable
evidence includes fitting reservation success, timeout atomicity, cancellation
permit recovery, natural close during an already-parked gap or oversized batch,
drain priority over expired retries, retained expiry and last-shell replacement,
fast-versus-stalled clients, actor-gone and failed-capture behavior, and detach
after wire retirement. Keep the real PTY convergence regressions as integration
coverage and report their actual results separately.

The close fanout runs client waits concurrently for one resource. Existing
child-before-parent cascade fanout still awaits each resource's complete fanout
before proceeding to its parent; this review does not claim isolation across
the entire cascade tree. Mailbox publication waits remain backpressure waits;
tracked task abortion and receiver closure provide their cancellation paths.
