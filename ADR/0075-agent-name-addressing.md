---
audience: contributors
stability: stable
last-reviewed: 2026-08-08
---

# 0075 — Agent names are addressable, and a withdrawn name is refused

**TL;DR.** The `name` every `phux.agent/v1` record carries becomes a selector
under a new `%name` sigil, resolved client-side against the agent index the CLI
already builds. A name addresses exactly one hub-local Terminal: ambiguity, an
unknown name, or an incomplete index refuses rather than narrowing silently.

Status: Proposed
Date: 2026-08-08

## Context

[ADR-0040](./0040-agent-identity-metadata.md) put agent identity in the
Terminal-scoped `phux.agent/v1` record, whose `name` is REQUIRED
([`docs/spec/L3.md`](../docs/spec/L3.md) §3.7);
[ADR-0046](./0046-server-side-agent-state-detection.md) gave it a writer. Every
agent pane has a stable human-facing name and no way to *type* it: every verb
takes `@N`, an opaque id that changes with every spawn and that nobody
remembers across a dozen panes.

[ADR-0021](./0021-control-plane-commands.md) resolves selectors client-side and
keeps the server selector-agnostic; the bare-name form is already the session
namespace. [ADR-0065](./0065-one-cli-grammar.md) asks for one grammar, not a
second spelling of one. [ADR-0071](./0071-what-phux-1-0-commits-to.md) freezes
the grammar, its exit codes, and the MCP arguments at 1.0 — a sigil is cheap
now and expensive later.

## Decision

1. **`%name` is the sigil.** `TARGET` gains `%<agent-name>` →
   `Selector::Agent(String)`; a bare `%` is a parse error. One sigil, one
   meaning: a bare name is a session, `:` and `.` locate a window and a pane,
   `@N` and `host/@N` are Terminal ids (`TerminalId`, `SatelliteTerminalId`),
   `#` is a tag set, `%` is an agent name. No bare-name overload, no `agent:`.

2. **Resolution is client-side, hub-local, and needs no wire change.**
   `Selector::Agent` resolves against the `TerminalId` → `AgentRecord` index
   `phux agent list` already builds. `phux.agent/v1` does not federate —
   `handle_get_metadata` reads the hub-local store and `phux-server/src/hub/`
   has no metadata arm, unlike `SUBSCRIBE_EVENTS` — so `%name` sees hub-local
   panes only, a satellite agent is an exit-1 miss, and uniqueness is checked
   hub-side. Federating L3 is out of scope. The server learns no new word.

3. **A name resolves to exactly one Terminal, or the verb refuses.**
   `pick_target_pane` is never applied to `%name`, including in `phux-mcp`'s
   `resolve_one`, which applies it unconditionally today and would narrow
   silently on a frozen surface. Unknown name: miss, exit 1. Two or more live
   records sharing it: refuse, exit 2, listing every candidate `@N`. An index
   not built completely: refuse as partial, exit 3 — "no pane holds that name"
   and "we did not finish looking" must not collapse into one silence. The cost
   is named, not assumed: `send-keys`/`paste` have no exit 2 today
   (`agents.md` §5.2) and gain one; `run` mirrors its child's status, so a
   refusal there stays exit 1 in words; exit 3 means "phux could not answer" —
   [ADR-0076](./0076-agent-prompt-and-lifecycle-wait.md)'s reading — not
   "retry".

4. **Uniqueness is enforced at resolve, and `%` addresses chosen names.** The
   addressable grammar is `^[a-z][a-z0-9_-]{0,31}$`, checked at parse time so a
   typo fails locally; the record's `name` stays "any non-empty string" per
   §3.7, so a display-style name is still valid, listed, and addressable by
   `@N` — just not by `%`, and `phux agent list` says which. No write-time
   check: L3 is last-writer-wins, so a scan two racing writers both pass is an
   O(panes) round trip, not a guarantee. Detector names are manifest constants
   (`name` defaults to `kind`), so twelve Claude panes all hold `claude` and
   `%claude` refuses as ambiguous.

5. **The write guard is a level read of the withdrawn shape.** `send-keys`,
   `paste`, `signal`, `run`, and any
   [ADR-0053](./0053-acknowledged-idempotent-input.md) acknowledged-batch verb
   refuse a `%name` target whose record carries a `kind` **and**
   `state: "unknown"` — exactly what `agent_state::withdraw_state` leaves, so
   it is positive evidence that the detector owned this pane and retracted. A
   record with no `kind` and `state: "unknown"` is the resting value of an
   identity-only declaration (§3.7: "An absent `state` means `unknown`") and
   resolves normally. This is a **safety gate and it reads the level**: a
   non-`unknown` state asserts only that no state-bearing rule contradicts it
   right now — absence of contrary evidence, equally true of a crashed pane —
   never that the occupant is who you named. Read-only verbs skip it.

6. **What that leaves unprotected, stated exactly.** A name never outlives its
   pane (§3.7 drops the per-Terminal store at close) and the agent-gone
   retraction ships (`runtime/client.rs` → `withdraw_state`). Nothing fires on
   a *kind change*: `apply_identity` returns `None` and `compose` preserves the
   prior `kind`, so claude→codex in one pane yields
   `{"name":"build","kind":"claude","state":"idle"}` and point 5's guard passes.
   A same-kind restart is undetectable: `identify.rs` discards the pgid.

7. **Two dependencies, named rather than assumed.** (a) The shipped Claude shim
   writes `--name claude --kind claude --state <x>` on every hook, making every
   Claude pane identically named *and* declared; it must default to
   `claude-${PHUX_TERMINAL_ID}` with a `PHUX_AGENT_NAME` override before
   `%name` ships. (b) Closing point 6's hole is an ADR-0046 change this ADR
   does not own: `Retract` on an identity change when the detector owns the
   record, a `withdraw_state` that also overwrites `kind` otherwise, and a
   `compose` amendment so a detector `kind` replaces a detector prior.

## Why

`%` is the only free sigil that is also shell-safe: `#tag` needs quoting in an
interactive bash, where `#` opens a comment, while `%` is literal in bash and
zsh outside job-control builtins. A grammar you cannot type unquoted is not one
grammar (ADR-0065), it is a grammar plus a footnote.

Point 3's refusal is load-bearing. `pick_target_pane` is right for a set-valued
selector, where the user asked for a representative. A name's entire value is
that it names one thing; narrowing silently means `phux send-keys %build
'rm -rf .'` lands in an arbitrary pane that shares a label.

Points 5 and 6 are weaker than the first draft, which claimed the detector
writes `unknown` on a kind change and so "needs no new machinery". The code
does the opposite, so that guard passed exactly when it needed to fire.

## Tradeoffs

- Resolving `%name` costs one `GET_METADATA` per pane where `@N` costs nothing.
- Point 3's third outcome is a client refactor, not reuse: the index moves into
  `phux-client` and `resolve_targets` gains a `Result` at every CLI and MCP site.
- Without point 7(b) an occupant swap silently retargets `%name`; with it, a
  same-kind restart still does.
- Two name grammars leave some records listed but unaddressable, a session
  named `%foo` unaddressable, and a hub and satellite `build` each look unique.
- An explicitly declared record locks the detector out (ADR-0046 point 8), so
  its staleness belongs to whoever declared it.

## Alternatives

**Bare `name`, or `agent:name`.** The bare form is the session namespace
(ADR-0021), so `phux kill build` would mean two things; `:` already separates
session from window, so `agent:build:2` is ambiguous and needlessly long.

**Reuse `#tag`.** Tags are set-valued and multiply assignable by design. Both
properties are wrong for a name, and conflating them erases the singular-target
refusal that is this ADR's main safety argument.

**A server-side name registry with bind-time uniqueness.** Stronger, rejected
for the reason ADR-0021 and [ADR-0017](./0017-tui-not-protocol-privileged.md)
gave: a server that resolves names is a server that parses selectors.

**Gate the write verbs on `state != unknown`.** The first draft's rule. §3.7
makes `unknown` the resting value of an identity-only record, so it refuses
those forever, and its escape — `--state idle` — locks the detector out.

**Mint a `stale` state, or a `quiescent` one.** Both add vocabulary with no
producer, and the withdrawn shape already separates never-derived from
derived-then-lost with no spec change.
