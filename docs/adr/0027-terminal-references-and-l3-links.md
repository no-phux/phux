---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-06-05
---

# 0027 — Terminals are referenced, not owned: views, links, and L3 tags

**TL;DR.** A Terminal is one server-side identity — one PTY (one winsize),
one libghostty grid (one reflow width). A *view* is a client-side reference
(`TerminalId`) placed in a layout slot; many views may point at one Terminal.
**Size is an identity property, not a per-view one** — concurrent views share
the Terminal's geometry; on disagreement a config-driven `window-size` policy
(default `smallest`) picks one and the others letterbox, as tmux does.
Per-view scroll is **deferred** (v1 mirrors share scroll). Tagging and
cross-Terminal *links* are L3 metadata (ADR-0015) keyed on `TerminalId`,
resolved client-side via a `#tag` selector — zero new wire surface. The
server never learns "view", "tag", or "link".

Status: Accepted
Date: 2026-06-05
See [ADR-0104](./0104-parent-bindings-are-l1-lifecycle.md) for the parent relation: it moved to L1 lifecycle; `phux.link/v1` stays for advisory, client-defined relations.

## Context

Selectors (ADR-0021) resolve session/window/pane — none of which are wire
concepts (ADR-0017) — client-side to a set of `TerminalId`s; the server only
knows Terminals. A "pane" is already just a client-side layout slot pointing
at a `TerminalId`, so nothing stops two slots (or two clients) referencing
one Terminal. The substrate (PTY + libghostty grid) is server-authoritative
and forwarded as VT bytes (ADR-0013); the view is a client projection.

Two questions fall out, and `selector.rs` is where they surface: (1) if many
views share one Terminal, what may each view vary *independently*? (2) how do
Terminals reference/tag/link each other ("group these two", "this follows
that") across panes and windows? Answering ad hoc risks leaking view/link
concepts onto the wire and re-coupling the server to layout — the coupling
ADR-0017 deliberately removed.

## Decision

1. **One Terminal = one identity = one geometry.** A `TerminalId` denotes a
   single PTY + single grid. Multiple views (slots/clients) may reference it;
   mirroring is allowed and cheap — the same VT byte stream fans out.
2. **Size is an identity property.** Concurrent views share the Terminal's
   authoritative `(cols, rows)`. On disagreement a `window-size` policy picks
   the size; others letterbox (larger) or clamp (smaller). **Default
   `smallest`** (nothing is ever cropped), **overridable via pure config** — a
   `phux-config` `window-size` key (`smallest` | `largest` | `latest` |
   `manual`). We never reflow one grid to two widths.
3. **Per-view scroll is deferred.** v1 mirrors share the Terminal's scroll
   position. A later pass MAY make scroll a per-view projection over the
   shared grid (valid only at the shared width); the door is left open, the
   work is not in this cut.
4. **Tags and links are L3 metadata** (ADR-0015) keyed on `TerminalId`:
   freeform string tags, plus `link` edges carried as a metadata record with
   a spec'd shape — `{ target: TerminalId, kind }` where `kind` is an **open
   enum** (v1 defines `group`; future kinds are additive). They ride the
   existing `SET_METADATA` / `SUBSCRIBE_METADATA` verbs — no new wire tag, no
   version bump. The server stores opaque bytes; the *schema* is normative
   (`docs/spec/`) so tag/link meaning cannot drift between clients.
5. **The selector grammar gains `#tag`**, resolving to the set of so-tagged
   Terminals, evaluated client-side against the snapshot + metadata exactly as
   `name:tag` resolves a window today (`@N` stays the raw-id form). The server
   stays selector-agnostic.

## Rationale

Keeping the server identity-only (ADR-0017) is the load-bearing choice: views,
tags, and links are all projections over `TerminalId` + L3 metadata, so a big
user-facing feature costs zero new wire surface and the server stays dumb and
testable. The size constraint is not a design we picked but an intrinsic
property of one-PTY-one-grid; naming it in an ADR prevents recurring "why
can't I render it bigger over there" churn. tmux — the incumbent — reached the
same conclusion (`window-size: smallest|largest|latest|manual` + letterbox),
strong evidence the constraint is real, not a phux shortcut, and why we mirror
its policy vocabulary rather than invent one.

## Tradeoffs

- We gain mirrored views and a tag/link graph for "free" (no wire growth) —
  but inherit tmux's constraint: **no two-sizes-at-once**. Letterboxing the
  non-authoritative views is the honest, documented behavior, not a bug.
- Default `smallest` never crops content; users wanting a different trade set
  `window-size` in config. `manual` implies a future resize verb (out of
  scope here; named so the enum value is not a later surprise).
- Spec'ing the L3 tag/link schema (key namespace + link record) keeps the
  bytes opaque to the server yet pins semantics for every client — the one
  drift risk of client-interpreted metadata, closed up front.
- `#tag` resolution yields a *set* (like a session resolving to many
  Terminals), so callers already handle multi-resolution — no new fan-out.

## Alternatives considered

**Per-view reflow (two grids for one PTY)** — rejected: a PTY has one winsize
and the child renders to one size; two grids desync content, double libghostty
cost, and have no coherent input/cursor model.

**Server-side tags/links as first-class wire entities** — rejected: violates
ADR-0017 (server stays Terminal-only); L3 metadata already carries arbitrary
client-defined structure, so the wire need not grow.

**Make "pane" a server concept so it can own per-view size** — rejected:
re-introduces exactly the layout-on-server coupling ADR-0017 removed.

**Per-view scroll in v1** — deferred, not rejected: it needs the viewport to
be a pure client-side projection that never mutates the shared grid; worth
doing, but its own change, and mirror-shared-scroll v1 is correct without it.
