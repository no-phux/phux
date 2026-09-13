---
audience: contributors
stability: stable
last-reviewed: 2026-07-15
---

# 0050 — Explicit spawn ownership, client-owned placement

**TL;DR.** `SPAWN_TERMINAL` may name an existing local Terminal as an
ownership address, forcing the server to host the new Terminal in that exact
window. Geometry remains an L3 client concern: headless callers publish the new
leaf through shared LayoutOps without publishing focus. An absent owner keeps
legacy behavior.

Status: Accepted
Date: 2026-07-15

## Context

A headless `phux spawn` connection is not attached, so the server previously
placed its Terminal in a recently active session. That heuristic can select a
different session from the target the caller intends to split. A follow-up L3
layout write cannot repair wrong registry ownership: the Terminal already
belongs to the wrong session/window.

Server-tracked focus is not an answer. ADR-0019 makes focus client-local, and
layout geometry belongs to the TUI metadata convention rather than L1.

## Decision

Add optional `SPAWN_TERMINAL.owner_terminal` field id 8. When present, the
server resolves that existing local Terminal and creates the new Terminal in
its exact owning window. Resolution failure is handled as `SPAWN_FAILED`; there
is no heuristic fallback. Ownership targeting is local-only and cannot be
combined with satellite routing.

The field conveys ownership only, never split direction, ratio, or focus.
After a successful spawn, a layout-aware caller uses the shared LayoutOps
read/mutate/write path to insert the returned leaf. Headless insertion preserves
the serialized active-window and focus fields. Layout coordination remains
last-write-wins as accepted by ADR-0019; no generic compare-and-set is added.
After the spawn reply, a placement caller re-reads authoritative state and
verifies that both the owner and new Terminal resolve to the expected exact
window/session before publishing layout. This detects an older additive decoder
that legally ignored field 8. An absent or mismatched pane is removed
synchronously and reported as unsupported/mismatched. If layout publication
returns a handled error, the caller likewise removes the known spawned Terminal
before returning failure.

`owner_terminal = None` preserves the existing attached-client / recently
active placement policy for legacy consumers and unplaced CLI calls.

## Consequences

An explicit target cannot land in another session or window, while L1 still
contains no layout or focus vocabulary. Older senders omit field 8 and retain
their behavior; field-tagged decoders skip or default the additive field.
Satellite placement remains intentionally unsupported until ownership and L3
layout metadata have a federation-safe addressing model.

## Alternatives

A geometry-free spawn followed only by L3 placement was rejected because it
cannot change the server registry ownership established at spawn time.

A session id field was rejected because the Terminal target already identifies
the exact window and avoids adding another non-federation-routable identity.

Server-side focus tracking or layout execution was rejected by ADR-0019.

## References

- [ADR-0019](./0019-tui-multi-pane-rendering.md)
- [ADR-0049](./0049-client-local-focus-and-advisory-attention.md)
- [L1 terminal lifecycle](../spec/L1.md#31-spawn-resize-and-close)
