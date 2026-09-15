---
audience: contributors
stability: stable
last-reviewed: 2026-09-14
---

# 0129 — Named projections are a metadata-key convention, not a resource

**TL;DR.** A "named shared projection" is not a new wire concept, resource
kind, or server-durable object: it is a consumer choosing which
`<prefix>.layout/v1/<session>` L3 metadata key to read and write, using the
schema `docs/spec/L3.md` §3.2 already defines. Durability is the workspace
archive, not the server. This closes the PHA-406 phase-2 candidate "named
shared projections as durable resources" with the smallest honest answer.

Status: Accepted
Date: 2026-09-14

## Context

PHA-406 asked whether phux should let scripts and consumers save and later
reattach to a *named* shared pane arrangement, distinct from the TUI's own
working layout. `docs/spec/L3.md` §3.5 already told alternative consumers to
use their own key prefix instead of the TUI's; ADR-0049 rejected `FOCUS_PANE`
and other layout commands on the wire, keeping layout as L3 metadata with no
protocol surface. `phux workspace save` captures the split tree into its
archive (`crates/phux/src/commands/workspace/archive/model.rs`), but
`workspace restore` only replayed one preferred pane per session — the split
tree was captured and thrown away. Separately, the metadata store had no
per-key size cap despite `docs/spec/L3.md` §2 recommending one since it was
written, and an ordinary (non-keep-empty) session reap left its
`phux.tui.layout/v1/<id>` key behind forever — only a keep-empty session's
reap ever cleaned it up (`crates/phux-server/src/state/reap.rs`).

## Decision

1. **No `Projection` resource kind.** A projection's entire identity is the
   metadata key a consumer chooses: `<prefix>.layout/v1/<session-id>`, the
   same schema `phux.tui.layout/v1/<session>` already uses. `insert-pane`,
   `move-pane`, `swap-pane`, and the placement flags on `spawn`/`launch`
   accept `--projection KEY` naming one; omitting it keeps the shared
   default the TUI and Cockpit both read on purpose. `LayoutOps::with_key`
   is the one client-side seam this adds.
2. **No server-durable named layout.** The only thing that survives a
   restart is a file the consumer owns: `phux workspace save`/`restore`.
   Restore now replays the archived split tree into the target session's
   default envelope with fresh window identities, rather than placing one
   preferred pane and discarding the rest. This is consistent with
   ADR-0092: the server holds no durable state of its own.
3. **Hardening, not new scope.** The server now enforces
   `limits.metadata-value-bytes` (default 256 KiB, `docs/spec/L3.md` §2's
   long-standing recommendation) before storing any value, and deletes every
   `<prefix>.layout/v1/<id>` key naming a session when that session reaps to
   zero windows, not only when it is kept empty (ADR-0105).
4. **The reference TUI never adopts a foreign key.** It recognizes only its
   own `phux.tui.layout/v1` prefix; any other `--projection` key is
   invisible to it by construction, not by a runtime check.

## Why

The only evidenced need was "let a script address its own arrangement
without stepping on the TUI's" — the metadata schema already generalizes to
that once a consumer picks its own prefix, exactly as §3.5 says. Naming that
choice `--projection` and validating the key shape is the whole feature; a
resource kind would add a lifecycle, an inventory row, and wire discriminants
for something with no process, no stream, and no server-side identity beyond
a string. The archive is durability that already existed and worked for
everything except the split tree itself, which was a bug, not a missing
feature.

## Tradeoffs

A named projection is exactly as durable as the metadata store: it does not
survive a server restart, and two consumers racing the same key still get
last-write-wins with no compare-and-swap. Consumers that want durability
must save and restore an archive explicitly; this is a deliberate floor, not
an oversight (point 2). Cross-session `move-pane` with `--projection` must
name both the source and destination envelope, since each key embeds its own
session id — passing exactly one is refused rather than guessed.

## Alternatives

**A `Projection` resource kind, listed and killable like a Terminal.**
Rejected: nothing about a projection has a lifecycle, a process, or a stream
to attach to; the kind-tag sprawl would buy addressing that a metadata key
already provides for free.

**Server-held named layouts that survive restart.** Rejected as the first
durable server state phux would ship, contradicting the coordinator-owns-
durability line ADR-0092 draws and PHA-406's own R10 finding that no durable
event journal belongs in `phux-server`.
