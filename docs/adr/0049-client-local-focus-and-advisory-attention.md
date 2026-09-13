---
audience: contributors
stability: stable
last-reviewed: 2026-07-15
---

# 0049 — Client-local focus and advisory agent attention

**TL;DR.** Shared layout metadata carries window and pane topology, never focus
authority. Each client preserves its own active window and per-window focused
pane when topology changes, repairing invalid focus deterministically. An
agent's pending question uses the existing terminal-scoped `Asked` event as
advisory attention. Automatic focus-following is a client opt-in that defaults
off. Layout operations remain L3 metadata with no wire change.

Status: Accepted
Date: 2026-07-15

## Context

[ADR-0019](./0019-tui-multi-pane-rendering.md) made layout shared but focus
per-client. The current v2 layout envelope nevertheless contains
`focused_window_index` and one `focused_terminal` per window because it grew
from the TUI's in-memory `Workspace`. The reconciliation path replaced the
whole local workspace with that envelope. A sibling's resize therefore also
adopted its serialized focus and could move another human's viewport, contrary
to ADR-0019.

The agent-workspace surface also needs to express "this pane needs a human."
That is easy to mistake for shared focus or for input authority. They are
separate decisions: [ADR-0033](./0033-input-authority-and-process-signals.md)
arbitrates who may type into a Terminal, while attention asks a consumer to
make a Terminal noticeable.

## Decision

1. **Focus remains client-local.** A layout metadata read adopts window order,
   names, split trees, ratios, additions, and removals. It does not adopt the
   writer's active-window index or per-window focused terminals. The reader
   preserves each valid local focus by window index and preserves its active
   index while that index remains valid.

2. **Repair is deterministic.** If a local focused terminal is missing from an
   updated window, that window focuses its first leaf in left-to-right
   depth-first order, matching ADR-0019's attach default. If the local active
   index is no longer valid, it clamps to the last surviving window; an empty
   workspace uses index zero. The serialized focus fields remain in envelope
   v2 for compatibility but are non-authoritative hints that the reference TUI
   ignores during reconciliation.

3. **Attention is advisory, not focus.** Attention means a process wants a
   human to notice its Terminal. A consumer may badge it, notify, or offer a
   jump action without changing the current viewport. It does not grant input
   authority and does not converge the viewports of attached clients.

4. **Reuse `AgentEvent::Asked`.** The current concrete attention case is an
   agent blocked on a human-answerable question. `Asked` already carries the
   question and suggestions, and its enclosing `EVENT.terminal` identifies the
   actionable Terminal. It already has subscription gating and the tested
   unknown-event compatibility described by
   [ADR-0035](./0035-agent-asked-event.md) and source precedence from
   [ADR-0036](./0036-agent-asked-detection.md). No second event is allocated.
   A future non-question attention semantic requires its own evidence and
   decision rather than overloading `Asked` silently.

5. **Directed focus defaults off.** A consumer may offer a local
   `focus-follows-agent` policy, but it is disabled by default. When enabled,
   that consumer may react to `Asked` by selecting the event's Terminal in its
   own workspace. An agent cannot direct focus by writing another client's
   layout focus fields.

6. **Layout operations stay in L3 metadata.** Programmatic split, resize,
   move, and window-tree edits use the existing versioned layout blob and
   `SET_METADATA` / `METADATA_CHANGED`, preserving the binary split-tree model
   from [ADR-0012](./0012-binary-split-tree-layout.md). Focus movement remains
   local. This decision allocates no frame, command, event tag, or capability
   bit and does not change the protocol version.

## Why

A metadata writer may be another human client, an agent, or automation. Giving
any writer implicit viewport authority turns an otherwise harmless resize into
an interruption and makes an L3 blob a hidden control channel. Topology sharing
still provides collaborative layout without that surprise.

`Asked` is narrower and cheaper than a generic attention event, and it exactly
matches the blocked-agent requirement already implemented. Keeping automatic
navigation as a local opt-in lets headless consumers ignore it and lets each
human choose interruption policy independently.

## Tradeoffs

The v2 envelope continues to serialize fields readers intentionally ignore.
Removing them would require a schema version and migration for no behavioral
benefit. Window focus is preserved by index because the envelope has no stable
window identity; a reorder can therefore cause deterministic first-leaf repair.

Users who enable focus-following accept viewport interruption on `Asked`.
Different attached clients may choose different policies and remain on
different panes by design.

## Alternatives

**Treat serialized focus as directed focus.** Rejected. It makes every layout
write capable of moving a human and contradicts ADR-0019's per-client focus.

**Allocate a generic `Attention` event now.** Rejected. The only evidenced
case is a pending question, already represented by `Asked`; a new tag would
add wire surface without a distinct payload or lifecycle.

**Add `FOCUS_PANE` or layout-operation commands.** Rejected. Focus is a
consumer projection, and layout already has an L3 coordination path. New
commands would privilege the reference TUI and reopen decisions settled by
ADR-0019.
