---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0017 — The reference TUI is not protocol-privileged

**TL;DR.** The reference TUI is one consumer among several with no protocol-level standing. The wire defines no message, type, or capability for the TUI's benefit alone — "window," "pane," "layout," "split," "focus," "status bar," "prefix" are TUI conventions implemented via L3 metadata. tmux control mode is one more consumer; the reserved `CC_FRONTEND` capability bit from ADR-0010 is reclaimed.

Status: Accepted
Date: 2026-05-26

## Context

phux ships a tmux-shaped reference TUI as a first-party consumer.
Today the wire and the TUI's product shape are entangled: the wire
talks about sessions, windows, panes, layouts, and focus because the
TUI does. Other consumers (agents, native GUIs, recorders) inherit
this vocabulary whether they use it or not.

[ADR-0015](./0015-protocol-layering.md) layers the protocol so the
substrate stops carrying TUI vocabulary. [ADR-0016](./0016-terminal-id-as-wire-primary.md)
renames the wire identity for a managed terminal. What remains is the
positioning question: **is the reference TUI a privileged consumer of
the protocol, or one consumer among several with no special standing?**
This ADR settles that.

## Decision

The reference TUI is **one consumer among several**. It has no
protocol-level privileges:

- **The wire defines no message, type, or capability that exists for
  the TUI's benefit alone.** "Window," "pane," "layout," "split,"
  "focus," "status bar," "keybinding," "prefix" — none of these are
  wire concepts. They are TUI-consumer conventions, documented in the
  TUI's design doc and implemented via L3 metadata.

- **The TUI consumes the same conformance tiers any consumer would.**
  L1 for terminals and their events; L2 for the Collection it
  presents as a "session"; L3 for the metadata keys that hold its
  layout tree, window ordering, focused-terminal pointer, and any
  other TUI-private state.

- **A future native GUI consumer ships against the same protocol with
  no new wire surface.** It mounts L1 (terminals it draws), L3 (its
  own metadata keys; need not share schema with the TUI), maybe L2
  (if it wants the session concept).

- **An agent SDK consumer ships against the same protocol with no
  new wire surface.** L1 only. Never sees the TUI's keys. Never
  knows the TUI exists.

- **`tmux control mode`, when added, is not the canonical "alternative
  frontend."** [ADR-0010](./0010-frontend-agnostic-tmux-cc-reserved.md)
  treated it as a special reserved consumer. Under this ADR it is
  exactly the same kind of thing as the native TUI and the GUI: a
  consumer that picks tiers and conventions. The `CC_FRONTEND`
  capability bit becomes redundant and is reclaimed.

## Rationale

- **The wire is the contract.** A protocol that privileges a specific
  consumer is a protocol that has hard-coded a product. We've
  watched tmux's wire calcify around tmux's product for 20 years;
  the move that escapes that future is to keep the wire substrate-
  shaped and let the products evolve client-side.

- **Multiple consumers is the point.** The vision (VISION.md) is
  built on the premise that the substrate matters more than any one
  presentation. Privileging the TUI would silently re-introduce the
  premise that "the multiplexer's job is to be a TUI."

- **Evolution lands cleanly.** When the TUI grows a new feature
  (tabbed layouts, floating overlays, pinned terminals), the wire
  doesn't change — just a new metadata key. When the substrate
  grows a new feature (a new terminal event, a federation
  refinement), the TUI inherits it like any other consumer.

## Tradeoffs

- **The TUI's design doc gets larger.** Every convention the TUI
  uses (the layout-tree schema, the window-ordering key, the focus
  key, the status-bar widget contract) is now documented as a TUI
  thing, not a phux thing. That doc has to be precise enough that
  another implementation could shadow the TUI by reading the doc and
  obeying the same metadata schema. Worth the cost.

- **No protocol-level "this is the official TUI" handshake.** A
  malicious or buggy alternative TUI can corrupt the metadata blob
  the reference TUI relies on. We mitigate with versioned metadata
  keys (`phux.tui.layout/v1`) and conflict-resolution rules the TUI
  documents. Same problem any shared-state app has; same solutions.

- **One less reserved capability bit.** ADR-0010's `CC_FRONTEND`
  loses its meaning. Free up the slot.

## Consequences for existing docs

- [ADR-0010](./0010-frontend-agnostic-tmux-cc-reserved.md) — Re-anchored:
  the "frontend-agnostic" decision still holds. The "tmux CC reserved
  as compat" portion is now redundant; CC is one consumer, no more, no
  less. Update the ADR's status to note the refinement.
- **SPEC.md** — `CC_FRONTEND` capability bit reclaimed.
- **TUI design doc** — Gains a "Metadata conventions" section that
  documents every L3 key the TUI reads or writes. Becomes the
  shadow-implementation contract.

## Alternatives considered

- **Privilege the reference TUI with a "canonical" capability bit.**
  Tempting because it makes shared-state coordination easier (only
  the canonical TUI may write certain keys). Rejected: it
  re-introduces the "protocol knows about products" pattern this
  ADR exists to refuse. Same goal (avoiding metadata corruption)
  can be achieved with versioned keys and a documented protocol on
  the metadata side.

- **Split the reference TUI out into its own repository.** Considered;
  the layering would be enforced by the file-system boundary.
  Rejected for v0.1: shipping the substrate without a working
  consumer is a credibility problem; shipping the consumer in-tree
  proves the substrate is real. Revisit when the SDK consumer is
  also in-tree and the TUI's specialness has eroded naturally.
