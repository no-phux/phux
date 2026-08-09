---
audience: contributors
stability: stable
last-reviewed: 2026-08-08
---

# 0077 — The agent read surface: sources, soft wrap, and truncation

**TL;DR.** phux grows no read-source vocabulary. The `snapshot` knobs keep
their meaning and what is missing rides back as additive `ScreenState` keys:
optional soft-wrap indices, a truncation marker, and the pane title. Every
match path unwraps when the server supplies wrap data. The alternate-screen
harvest is split out to [ADR-0078](./0078-alternate-screen-history.md).

Status: Proposed
Date: 2026-08-08

## Context

`phux snapshot` reads a pane through `GET_SCREEN`, returning a `ScreenState` of
viewport rows plus optional scrollback and sparse cells
([ADR-0022](./0022-tool-for-agents.md) §2,
[`../docs/spec/L1.md`](../docs/spec/L1.md) §6.1). Three gaps sit in that shape.

`wait --until` substring-matches viewport rows client-side, so a match
straddling a soft wrap silently never fires — the row the emulator painted is
not the line the program wrote. `ScreenState` cannot say that a requested
window dropped older rows, so a caller cannot tell a short transcript from a
clipped one. And the pane's OSC 0/2 title is on no read at all: neither
`GET_SCREEN`'s payload nor `GET_TERMINAL_STATE`'s carries it.

A fourth gap — full-screen agents keep their transcript in the alternate
screen, where no `--scrollback` value reaches it — was drafted here and is now
[ADR-0078](./0078-alternate-screen-history.md): a multi-week subsystem that
writes to the PTY, which should not share a `Status:` line with additive JSON
keys. Nothing below depends on it.

## Decision

1. **No read-source vocabulary.** `--scrollback[=N]` and `--cells` keep their
   meaning; phux does not grow a named `visible | recent | detection` enum over
   knobs that already express the same thing; what is missing is added as
   orthogonal modifiers. The detector's region slices
   ([ADR-0046](./0046-server-side-agent-state-detection.md) §4) stay
   `pub(crate)` behind the offline `agent explain` facade that already prints
   them — a manifest-debugging surface, not a fourth way to read a pane.

2. **Soft-wrap indices travel as an optional field; joining is consumer-side.**
   `ScreenState` gains `soft_wrap: Option<SoftWrap>`, where `SoftWrap` is
   `{ lines: Vec<u32>, scrollback: Vec<u32> }` — indices of rows continuing
   onto the next, from libghostty's per-row wrap bit. The optionality is the
   contract, not a convenience: `None` means *this server does not compute wrap
   data*; `Some` with empty vectors means *this screen has no wrapped rows*. A
   version number cannot express that difference — it describes the server, not
   the payload.

3. **Every match path unwraps when wrap data is present, and says so when it is
   not.** `snapshot` renders as painted (`--unwrap` joins); `wait --until`
   joins wrapped runs before matching, which fixes the straddling-match bug
   with no new condition variant ([ADR-0022](./0022-tool-for-agents.md) §4).
   Against a server that sends `None`, `wait` matches as painted — today's
   behavior, not a new silent failure — and its `--json` timeout report names
   the degradation.

4. **`ScreenState` gains `truncated: bool` and `truncated_reason:
   Option<String>`, meaning exactly one thing:** the requested window dropped
   older rows. This pair is not a general-purpose partial-result channel.
   [ADR-0078](./0078-alternate-screen-history.md) mints its own key for a
   refused harvest rather than overloading these.

5. **`ScreenState` gains `title: Option<String>`.** It is pane chrome: a consumer
   rendering a pane header needs it, and `agent explain --file` currently
   captures a screen and loses the title that was on it. It is deliberately
   *not* material for client-side state derivation —
   [ADR-0046](./0046-server-side-agent-state-detection.md) rejected that, and
   [`../docs/spec/L3.md`](../docs/spec/L3.md) §3.7 requires consumers to prefer
   the published `phux.agent/v1` record over heuristics on the title.

6. **`SCHEMA_VERSION` stays `3`, and every new field carries
   `#[serde(default)]`.** [`../docs/consumers/agents.md`](../docs/consumers/agents.md)
   §4.1 states the contract while explaining why `attached_clients` arrived
   without a bump: adding a key is non-breaking because consumers ignore
   unknown keys, so the version moves only when a key is removed, renamed, or
   retyped. Adding four keys is therefore not a bump. Naming it matters because
   `crates/phux-core/src/screen.rs` records `2` and `3` as bumps for purely
   additive fields — the struct's own history is looser than the contract, and
   the contract governs. `#[serde(default)]` makes the other direction safe: a
   new consumer against an old server would otherwise hit a hard deserialize
   error rather than a missing key.

7. **No wire change.** These ride inside the existing
   `COMMAND_RESULT { OK_WITH(JSON(..)) }` payload; unwrapping is consumer-side.
   No `PROTOCOL_VERSION` bump and no `ServerFeature` bit — `soft_wrap`'s
   optionality already carries the only capability signal a consumer needs.
   `agents.md` §4.2's field table and the `ScreenState` doc comment change
   with it.

## Why

**Absence must be distinguishable from emptiness.** The soft-wrap fix is only
worth having if a client can trust it. A bare `Vec<u32>` defaulting to empty
would make an old server look exactly like a screen with no wrapped rows, so
the new client would silently miss the straddling match it was built to catch —
reintroducing the original bug with no signal at all. An `Option` answers the
question at the payload, which is where the consumer is standing.

**Flags, not joined text, for soft wrap.** Replacing painted rows with joined
ones breaks every consumer that indexes by row, the detector's region extractor
included; sending both doubles the payload. Only the consumer knows whether it
wants rows as painted or as written.

**The convenience surface is a narrow claim.** A consumer that runs its own
engine ([ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md) §4)
already holds the wrap bit in its replica and needs nothing here. `GET_SCREEN`
is the convenience read for consumers that run no engine, which ADR-0030 §2
permits in the same paragraph that forbids growing structured wire surfaces.
That is the argument for this addition; "only the server knows this" is not.

## Tradeoffs

- **Four keys on a struct [ADR-0071](./0071-what-phux-1-0-commits-to.md) is
  about to freeze**, none withdrawable after 1.0. `soft_wrap`'s optionality is
  the one shape here that is hard to change later.
- **`title` widens what a determined consumer can scrape** — nothing stops it
  being used for the client-side derivation ADR-0046 rejected. The answer is
  documentary, not structural, and it will drift.
- **Holding `SCHEMA_VERSION` at 3 makes the struct's history inconsistent:**
  versions 2 and 3 were additive bumps and this one is not.

## Alternatives

**A named source vocabulary (`visible | recent | recent-unwrapped |
detection`).** Rejected: it re-spells existing knobs, freezes four combinations
into an enum on a surface ADR-0071 is about to freeze, and makes unwrapping a
source rather than a modifier — which forces the same choice on `cells`,
`truncated`, and everything added later.

**Bump `SCHEMA_VERSION` to 4 instead of making `soft_wrap` optional.**
Rejected: the version tells a consumer what the server can do, not what this
payload contains, so it cannot answer "are these vectors empty because there
are no wrapped rows?". It also contradicts `agents.md` §4.1's rule for a purely
additive change, and a rule that bends for convenience stops being a signal.

**Server-side joining, returning unwrapped `lines`.** Rejected: it retypes a
field every existing consumer indexes by row.

**Keep the alternate-screen harvest in this ADR.** Rejected, and that is why
this document was rewritten. Points 1–6 need no wire change, no PTY write, no
lease, and no new spec role; the harvest needs all four. Sharing one `Status:`
line would have made the cheap, correct part wait on the expensive, contested
one. See [ADR-0078](./0078-alternate-screen-history.md).
