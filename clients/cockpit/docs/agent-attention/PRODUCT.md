---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---
# Agent attention and terminal intervention

**TL;DR.** A blocked agent directs attention to its exact parent terminal.
Inspection shows the latest state-bearing producer evidence; the operator
returns to that terminal and answers its real TUI. Only subsequent producer
records reconcile the displayed state. This is bounded live evidence, not a
durable work history.

## Workflow

1. An AgentSession resource appears under its exact parent ResourceId, including
   when the agent starts after Cockpit attaches.
2. A producer ask or permission/elicitation record marks that parent as needing
   attention, even when another split in its tab is focused.
3. Agent inspection is reachable by pointer and keyboard in every window. It
   displays exact resource and parent identities, provider/native identity,
   catalog versus stream state, and the latest state-bearing record's reason,
   sequence, and coordinator-stamped record time when present. Missing evidence is explicit.
4. Jump to parent selects the exact terminal and restores terminal input.
   Stale navigation cannot select a replacement at the same tab position.
5. The operator answers the agent in its actual terminal. Inspection and jumping
   do not acknowledge, dismiss, approve, or otherwise clear the ask.
6. A later producer record changes state and updates evidence, including a new
   reason while still blocked. Closing the resource removes it. Reconnection
   does not carry evidence from a replaced stream generation.

## Presentation and boundaries

- One inspected resource at a time, with bounded paging and explicit totals;
  no silent global row cutoff. Long reason text is bounded and visibly truncated.
- Timestamp is coordinator record evidence, not an invented local receipt or freshness
  guarantee. Offline resources must not claim actionable live attention.
- Native Approve is absent: current APIs lack an atomic current-ask answer
  contract. Phux owns membership, execution, identity, and stream ordering.
- Local scratch terminals remain ephemeral. Objective/Run/Artifact coordination
  remains the separate proposed durable-work architecture.

## Acceptance

- Behavioral regressions cover repeated blocked reasons, retained replacement,
  stale live delivery, malformed batches, and retained batches above 64 KiB.
- Shipping markup tests cover keyboard inspection, all windows, exact parent
  navigation from a nonfocused split, and unchanged attention after inspection.
- A real PTY producer opens an agent using its own parent identity, emits an ask,
  consumes actual terminal input, then emits a computed result and new state.
  Live acceptance verifies the computed result in the intended PTY and absent
  elsewhere, and verifies resource birth, evidence update, and retirement.

See [technical design](TECH.md) and
[product direction](../PRODUCT_DIRECTION.md).
