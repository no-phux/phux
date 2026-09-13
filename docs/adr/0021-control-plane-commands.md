---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0021 — Control-plane commands and client-side selector resolution

**TL;DR.** The CLI's `ls` / `new` / `kill` verbs ride the generic
`COMMAND` / `COMMAND_RESULT` envelope (SPEC §5), not new dedicated
frames. Session / window / pane selectors are resolved **client-side**
against a `GET_STATE` snapshot; the only commands that cross the wire are
Terminal-scoped L1 commands (`GET_STATE`, `SPAWN`, `KILL_TERMINAL`). No
session or window concept enters the wire — [ADR-0017](./0017-tui-not-protocol-privileged.md)
holds. The L2 Collection that will eventually own the durable "session"
identity is named as the forward path; its command surface is deferred.

Status: Accepted
Date: 2026-05-28
Superseded in part by [ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md): the L2 Collection forward path; the durable session is L3 metadata plus client logic, and the atomic group operation is KILL_TERMINALS.

## Context

phux ships CLI subcommands beyond `attach` / `server`: `phux ls`
(list sessions), `phux new` (create a session), `phux kill TARGET`
(destroy a session / window / pane). The issue tracker (phux-k61.2,
phux-5cd, phux-3kj) frames these as session-scoped, and the selector
grammar (`docs/consumers/tui.md` §3: `.`, `name`, `name:N`,
`name:N.M`, `name:tag`, `@N`) names sessions, windows, and panes.

But [ADR-0016](./0016-terminal-id-as-wire-primary.md) made the
Terminal the wire primary and [ADR-0017](./0017-tui-not-protocol-privileged.md)
removed session / window / pane / layout / focus from the wire
entirely — they are reference-TUI conventions, realized as an L2
Collection (the "session") plus L3 metadata (window ordering, layout,
focus). The wire's control surface is the generic `COMMAND` envelope
(SPEC §5), whose catalog (§5.1) is Terminal-scoped. That envelope is
specified but **not yet wire-implemented**, and L2 Collections are
reserved / TBD.

So the verbs are described in TUI vocabulary, but the wire has no
session or window. This ADR settles how they map.

## Decision

1. **The generic `COMMAND` / `COMMAND_RESULT` envelope is the phux
   control plane.** It is wire-allocated now (proto tier): `COMMAND
   { request_id, cmd }` → `COMMAND_RESULT { request_id, result }`,
   `request_id`-correlated and asynchronous (the server MAY interleave
   other frames before the result, per SPEC §5). All future control
   verbs ride it; we do not grow a parallel family of dedicated
   request/reply frames the way `SPAWN_TERMINAL` did.

2. **Selectors are resolved client-side.** A session / window / pane
   selector is parsed and resolved by the consumer against a
   `GET_STATE` snapshot, *not* sent to the server. The selector
   resolves to a set of `TerminalId`s (and, for a whole-session
   target, the Collection identity). Only Terminal-scoped commands
   then cross the wire. The server never parses a selector and never
   learns the word "session" or "window."

3. **v0.1 wires two L1 commands** behind the envelope:
   - `GET_STATE { scope }` → `OK_WITH(STATE(snapshot))` — backs
     `phux ls` and all selector resolution.
   - `KILL_TERMINAL { terminal_id }` → `OK` — backs `phux kill`,
     issued once per resolved Terminal.

   `phux new` does **not** mint a new command: it reuses the existing
   `ATTACH { CREATE_IF_MISSING { name, command, cwd } }` path (SPEC §7),
   which already creates a named session and attaches. The spec's
   collection-scoped `SPAWN` command is a *terminal-under-collection*
   operation (closer to "split" than "new session") and is left for the
   L2 milestone, not conflated with `new` here.

4. **The L2 Collection is the forward path for durable "session"
   identity**, but its command surface (`CREATE_COLLECTION`,
   `DESTROY_COLLECTION`, `COLLECTION_ID` values) is **deferred** until
   L2's tagged union is allocated. Until then the server's native
   session registry is the v0.1 backing store that `GET_STATE` /
   `SPAWN` read and write — an implementation detail behind the
   envelope, not a wire concept.

## Why

- **It keeps the wire substrate-shaped.** Adding `LIST_SESSIONS` /
  `KILL_SESSION` frames would re-privilege the TUI's product vocabulary
  on the wire — exactly what ADR-0017 refuses. Resolving selectors
  client-side puts the TUI's conventions where they belong: in the
  consumer.

- **It uses the envelope the spec already designed.** SPEC §5 defined
  `COMMAND` / `COMMAND_RESULT` precisely so control verbs would not
  each mint a frame pair. `ls` / `new` / `kill` are its first real
  callers; building the envelope now pays for every later verb
  (`run-hook`, `resize`, attach/detach-terminal) at zero marginal
  wire cost.

- **It unblocks the CLI without freezing L2.** The hard, open design
  (Collection lifecycle, multi-Terminal grouping, L3 layout schema)
  does not gate shipping visibility and teardown. v0.1 leans on the
  native session registry; the migration to Collections is additive.

## Tradeoffs

- **The consumer carries the selector grammar and a resolution pass.**
  A second consumer (a GUI) re-implements selector resolution, or we
  factor it into a shared crate. Acceptable: it is consumer logic by
  ADR-0017's definition, and the grammar is small.

- **`GET_STATE` returns the server's session-shaped snapshot in v0.1.**
  That snapshot leaks the native session model the rest of this ADR
  calls an implementation detail. We accept the leak transitionally;
  when L2 lands, the snapshot becomes Collection + Terminal shaped and
  the session framing moves fully client-side. The `scope` field is
  the seam that lets that evolve.

- **`kill` of a whole session is one round-trip** via `KILL_COLLECTION
  { collection, name }` (the teardown counterpart to `CREATE_SESSION`;
  phux-h9s). The server resolves `name` to its session and tears down
  every Terminal it owns in a single command. Sub-session targets
  (a window, a pane, a bare `@id`) still ride one `KILL_TERMINAL` per
  resolved Terminal — they address a strict subset, so a session-level
  command would over-kill.

## Alternatives

- **Dedicated session frames (`LIST_SESSIONS`, `CREATE_SESSION`,
  `KILL_SESSION`).** Fastest to ship and matches the issue text.
  Rejected: it contradicts ADR-0016/0017 by putting TUI vocabulary on
  the wire, and it forks a frame family the `COMMAND` envelope exists
  to prevent. The debt is a guaranteed future wire break.

- **Server-side selector resolution over a `COMMAND { KILL, target:
  str }` string DSL.** Rejected: SPEC §5.2 explicitly forbids a
  string-based command DSL; commands are a typed enum. A server that
  parses `name:N.M` is a server that knows about windows.

- **Block the trio on full L2 Collection design.** The honest
  end-state, but it stalls shippable CLI value behind the largest
  open design in the protocol. This ADR takes the additive path and
  names L2 as the explicit successor instead.
