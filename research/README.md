---
audience: contributors, agents
stability: stable
last-reviewed: 2026-05-28
---

# research/

**TL;DR.** Scratch tier per [`docs/CONVENTIONS.md`](../docs/CONVENTIONS.md).
Active reference notes live here; ratified findings move to
[`archive/`](./archive/) with a top banner pointing at the ADR that
absorbed them. Nothing in `research/` is authoritative — for current
behavior, follow the cross-link to the ADR or to the relevant
`docs/` reference doc.

## Files

- [`2026-05-25-awesome-libghostty-scan.md`](./2026-05-25-awesome-libghostty-scan.md) —
  competitive scan of the awesome-libghostty project list; what to steal
  and whether anything invalidates the SPEC or accepted ADRs.
- [`2026-05-25-libghostty-renderstate.md`](./2026-05-25-libghostty-renderstate.md) —
  capability survey of `libghostty-vt`'s `RenderState` read API and
  dirty-tracking model; the renderer-side contract phux drives in both
  client and server.
## archive/

Holds ratified-or-absorbed notes. Each one carries a banner linking to
the ADR or reference doc that supersedes it.

- [`archive/2026-05-26-state-sync-algorithm.md`](./archive/2026-05-26-state-sync-algorithm.md) —
  algorithm-composition study for long-arc wire semantics. Ratified by
  [ADR-0018](../ADR/0018-lazy-state-synchronization.md).
- [`archive/2026-06-23-agent-asked-capture-harness.md`](./archive/2026-06-23-agent-asked-capture-harness.md) —
  clean-room harness notes for collecting empirical agent-asked evidence.
  The capture surfaces it drove shipped as the agent-asked wire events;
  its companion script (`scripts/agent-asked-capture.sh`) was retired
  with it.
- [`archive/2026-06-23-launch-polish-qa.md`](./archive/2026-06-23-launch-polish-qa.md) —
  one-shot launch-polish QA sweep notes; the fixes it drove landed in
  June 2026.

## Conventions

- Every file declares `stability: scratch`.
- No file in `research/` is linked from any `stable` doc.
- When a note is ratified by an ADR or absorbed into a reference doc,
  move it to `archive/` with a banner pointing at the new home.
