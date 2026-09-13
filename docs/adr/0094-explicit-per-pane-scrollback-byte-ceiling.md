---
audience: contributors
stability: stable
last-reviewed: 2026-09-02
---

# 0094 — Per-pane scrollback is bounded in bytes, by phux, explicitly

**TL;DR.** `defaults.history-limit` is only libghostty's *line* limit, and the
engine prunes on whichever of its line and byte limits is reached first. A
terminal built through the C API keeps Ghostty's 10_000-byte constructor
default, so that byte limit — not `history-limit` — decided how deep a phux
pane's history was: about 810 rows at 80 columns and 295 at 200, whatever the
config said. phux now installs the byte limit itself, from a config key of its
own — `defaults.history-bytes`, 2 MiB — because retained history is not free
at attach time: the engine materialises every retained page when a client
bootstraps, so the operator is choosing scrollback depth *and* attach latency
with one number and deserves to see both.

Status: Accepted
Date: 2026-09-02

## Context

`TerminalActor::build` passed `defaults.history-limit` (50000) as
libghostty-vt's `TerminalOptions::max_scrollback`, which sets
`GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES`. Nothing set the sibling byte
limit, so it stayed at Ghostty's `Terminal.Options.max_scrollback_bytes`
default of 10_000 bytes, floored by the engine at two standard pages. Pruning
is page-granular, so the effective retention was one standard page of history
regardless of the configured line count. Measured on the pinned engine, at
100_000 written lines:

| `max_scrollback` | byte limit | rows retained (80 cols) | rows retained (200 cols) |
|---|---|---|---|
| 50000 | engine default | 810 | 295 |
| 2000  | engine default | 810 | 295 |
| 50000 | 2 MiB | 2669 | 943 |
| 50000 | 10 MiB | 14403 | 5703 |

The line limit was inert. The reason it cannot simply be made to work is on
the other side of the ledger: the native bootstrap's `detach_ready` acquires
the client-pullable history lease by encoding **every** retained history page
into owned records, synchronously, in one actor turn, on the attach critical
path. Measured at 200x50:

| byte limit | rows retained | `detach_ready` | transient host bytes |
|---|---|---|---|
| engine default | 229 | 2 us | ~0 |
| 2 MiB | 943 | 8 ms | 2.3 MB |
| 4 MiB | 2133 | 22 ms | 6.1 MB |
| 10 MiB | 5703 | 65 ms | 17.4 MB |
| 32 MiB | 19031 | 222 ms | 47.7 MB |

Retained history and attach latency therefore trade directly, per pane, per
attach, on the single server thread.

## Decision

**1. phux sets the byte limit.** `TerminalActor::build` calls
`set_scrollback_max_bytes` immediately after constructing the canonical
terminal, so neither bound is inherited from an engine constructor default.

**2. The byte limit is a config key: `defaults.history-bytes`, default 2 MiB.**
`defaults.history-limit` keeps its line semantics and its 50000 default, and
the two travel together as one `ScrollbackLimits` value from config through
server state to pane construction. `phux config check` rejects a
`history-bytes` above `MAX_HISTORY_BYTES` (64 MiB) with a located finding
rather than clamping silently.

This is a reversal of the first version of this ADR, which made 2 MiB a
private constant on the argument that the number was not usefully tunable. The
measurements below say the opposite: the curve is steep, monotonic, and
*legible* — every extra MiB buys a predictable ~475 rows at 200 columns and
costs a predictable ~6.5 ms of per-pane attach latency. That is exactly the
shape of a decision an operator can make better than a default can, because
only they know whether their session is one pane they attach to hourly or
twelve panes they attach to constantly. Withholding the knob does not spare
them the tradeoff; it only hides it.

Both keys are documented together, with the curve, everywhere the schema is
user-facing: `default.toml`, `docs/CONFIG.md`, `docs/consumers/tui.md`,
`docs/operations.md`, and the generated `docs/reference/config.md`. Documenting
`history-limit` without `history-bytes` is what made the original bug invisible
for as long as it was, so the pair is never described alone.

## Rationale

Two MiB is the smallest value that is a real bound at every grid width — the
engine floors its own byte limit at two standard pages, so anything lower is
indistinguishable from leaving the default in place — while keeping the
per-pane history lease inside single-digit milliseconds. It roughly triples
real retained history on a wide grid, and it makes the ceiling phux's stated
decision rather than a constant inherited from an engine constructor.

The bound is expressed in bytes rather than lines because bytes are what the
host actually spends. A row's cost depends on width, styles, graphemes and
hyperlinks, so a line limit bounds memory only for one particular kind of
content; a byte limit bounds it for all of them. That is also why the two keys
must be presented as a pair rather than as a primary knob and a footnote: a
user who reads only `history-limit` will set it to 200000, see no change, and
have no way to find out why.

The 64 MiB maximum is a latency bound, not a memory one. At 64 MiB a single
pane's history lease is already most of a second of blocked server thread, and
a session of those is an attach nobody would call working. Rejecting the value
at `config check` time tells the operator that; accepting and clamping it would
not.

## Tradeoffs

Retention costs attach latency and peak RSS, permanently. Measured on an
isolated server with four panes each carrying 60k lines of output at 188
columns, fresh -> seeded -> attached -> detached -> settled RSS:

| build | fresh | seeded | attached | settled | client handshake |
|---|---|---|---|---|---|
| no explicit byte limit | 15.7 MB | 22.7 MB | 58.6 MB | 58.6 MB | 0.2 ms + 46 ms |
| 2 MiB | 15.7 MB | 27.3 MB | 89.8 MB | 89.8 MB | 0.1 ms + 122 ms |

RSS does not come back after detach in either case: the engine's history
records are freed but the host allocator keeps the pages, so peak is what
matters, not steady state. Choosing 2 MiB therefore spends about 31 MB of
permanent high-water RSS and about 75 ms of four-pane attach latency to turn
295 retained rows into 943. Raising it further multiplies both.

Shipping the ceiling as a knob means a user can configure a slow attach for
themselves, and a 64 MiB session of panes is a visibly bad experience the
config accepts up to its maximum. That is the accepted cost of not hiding the
tradeoff: `config check`, the schema docs, `default.toml`, and this ADR all
carry the curve, so the choice is informed rather than blind. Raising the
shipped default toward herdr's 10 MiB remains gated on the engine leasing
history lazily instead of materialising it at READY.

## Alternatives considered

- **Leave it alone.** Cheapest attach, lowest RSS, but `history-limit` stays
  a value the engine ignores and a 200-column pane keeps 295 rows of history.
- **Make `history-limit` mean bytes.** Rejected: it is the tmux-shaped name
  and tmux users read it as lines. Silently changing its units would turn
  every existing config into a misconfiguration no error could catch.
- **Keep 2 MiB as a private constant** (the first version of this decision).
  Rejected on amendment — see "Decision", item 2. The argument was that attach
  latency is not something a user should have to price; the answer is that
  they are paying it either way, and a default cannot know their pane count.
- **Bound `DetachOptions` instead of retention.** The engine treats an
  exceeded detach budget as a hard error rather than a truncation, so a pane
  with deep history would fail its bootstrap instead of shipping less history.
- **Publish `BOOTSTRAP_READY` before acquiring the lease.** The engine's
  `detach_ready` consumes a capture that holds the terminal's mutation
  exclusion, so deferring it would stall live output for every client. This is
  the right long-term shape and belongs with the engine work in
  `phux-slogic`.
