---
audience: contributors
stability: stable
last-reviewed: 2026-08-08
---

# 0076 — Prompting an agent is acknowledged; waiting on one is event-driven

**TL;DR.** `phux agent prompt` submits through the acknowledged `APPLY_INPUT`
batch rather than fire-and-forget `ROUTE_INPUT`: text and Enter ride one
operation, so a partial write can only lose the submission, never the meaning.
`phux agent wait` is satisfied by an observed *transition*, never by a level
read of `idle` — the value phux publishes when nothing matched.

Status: Proposed
Date: 2026-08-08

## Context

[ADR-0046](./0046-server-side-agent-state-detection.md) gave `phux.agent/v1`
([ADR-0040](./0040-agent-identity-metadata.md),
[`L3.md`](../docs/spec/L3.md) §3.7) a server-side writer, so `state` is live.
Nothing reads it as a control-flow input, and nothing writes *to* an agent with
a receipt: `send-keys` and `paste` use fire-and-forget `ROUTE_INPUT`, `wait`
polls a screen, `watch` subscribes to events and never metadata, and
`APPLY_INPUT` ([ADR-0053](./0053-acknowledged-idempotent-input.md),
[`L1.md`](../docs/spec/L1.md) §6.2.1) shipped acknowledged with no CLI caller.

Fire-and-forget stops being acceptable when driving an agent: a prompt is not
idempotent at the receiver, a doubled one can run a destructive tool twice, a
dropped one burns a timeout the orchestrator blames on the agent, and the only
recovery — resend — produces the duplicate. Reading the result back is thinner
than it looks. The five shipped manifests declare five `working` rules and five
`blocked` rules and nothing else; `idle` is the fail-safe fallthrough ("no
state-bearing rule matched => Idle"), and `claude.toml` records why its authors
declined a positive idle rule.

## Decision

1. **`phux agent prompt TARGET TEXT` submits via `APPLY_INPUT`.** It requires
   `ACKNOWLEDGED_INPUT` in `HELLO_OK.server_caps`; an older server is refused
   (exit 2), never downgraded. `APPLY_INPUT` is local-only, so a satellite
   target ([ADR-0066](./0066-host-namespace.md)) is refused (exit 2). `agent
   wait` refuses one too: L3 does not federate, so it would report a healthy
   remote agent as absent.

2. **`OK` is a kernel-queue receipt, and each typed error has one reading.** It
   means `write_all` and `flush` completed on the PTY **master**: every byte
   accepted into the tty's input queue, strictly more than `ROUTE_INPUT` states
   and strictly less than consumption ([`L1.md`](../docs/spec/L1.md) §6.2.1).
   `INPUT_DELIVERY_UNKNOWN` is terminal — a same-id retry replays the cached
   unknown, a new-id retry is the duplicate this design prevents — so the CLI
   **exits 1** with the operation id, not 3, whose published meaning
   (`docs/consumers/agents.md` §5.2) is *retry is correct*. Pre-handoff refusals
   wrote nothing and may be retried unchanged under the same id:
   `RESOURCE_EXHAUSTED` (backoff, then exit 1), `INPUT_LEASE_HELD` (exit 2).
   `CANONICAL_LIMIT_EXCEEDED` also wrote nothing but cannot succeed unchanged
   (exit 2). `UNSAFE_PASTE` cannot arise: point 3 sends TRUSTED, which skips
   safety classification, deliberately bypassing the pane's untrusted-paste
   policy on caller-supplied text.

3. **Text and Enter are ONE batch, and the text is single-line.**
   `[Paste(trusted, text), Key(Enter)]`, the shape `send-keys` already builds,
   encoded against one mode snapshot and written as one PTY job with no submit
   delay. Enter last means a partial delivery drops the submission and leaves
   unsubmitted text on screen, the recoverable failure. Text with a raw newline
   is refused (exit 2): `paste::encode` turns newlines into carriage returns
   when the pane has not set DEC 2004, a mode the CLI cannot observe, so a
   multi-line prompt can otherwise become N submissions.

4. **Ownership is re-verified from the record, not a fresh syscall.** The CLI
   reads `phux.agent/v1` and refuses when it is absent or names a different
   `(kind, name)` than the target resolved to (exit 2). For a *detector-owned*
   record the staleness bound is one re-identification interval (~5 s, ADR-0046
   point 10) plus one detect tick, so a prompt can land in a shell that replaced
   the agent inside that window; the CLI holds its subscription across the
   submit and reports an identity change, a withdrawal to `unknown`, or a
   tombstone arriving before the result as delivery to an unknown occupant
   (exit 1). The bound holds only there. A record whose `state` was explicitly
   declared (ADR-0046 point 8), which phux's own Claude shim writes on every
   hook, stands the detector down: no staleness bound, no tombstone, and no way
   for a consumer to tell the two classes apart. `--json` reports the record the
   check passed on rather than claiming a freshness `prompt` lacks. The server
   gains no agent-aware precondition on `APPLY_INPUT`: detection fails safe
   toward `idle`, input must fail safe toward *delivery*.

5. **`phux agent wait TARGET --until STATE... --timeout MS` is satisfied by a
   transition, not by a level.** `SUBSCRIBE_METADATA` first, then one
   `GET_METADATA` recording the pre-wait value, then `METADATA_CHANGED`; the
   wait completes on an observed transition *into* a member of `--until`. It
   never completes on a level read of `idle`, which asserts only that no
   state-bearing rule matched and is equally true of a finished agent, a
   repainting TUI, a crashed one, and a pane running `less`. A completion gate
   firing on that returns success on a corpse — instantly, and on every pane
   with no manifest at all. The fast path may satisfy the wait only on a
   **positively asserted** level: `blocked`, which five shipped rules assert, or
   `done`, which no manifest emits and only an explicit lifecycle writer
   produces (phux's Claude shim, on `Stop`). Neither is reachable by
   fallthrough, so `--until done` means "wait for an instrumented agent to
   declare completion" and is inert elsewhere. Because `METADATA_CHANGED` is
   `try_send` and dropped on a full mailbox, and publication is edge-filtered
   (ADR-0046 point 7), the CLI re-reads `GET_METADATA` on the existing `wait`
   cadence under the same deadline and treats a value differing from the last it
   held as the edge it missed: level-triggered *recovery of an edge*, not a
   level gate. `--until` repeats and ORs, defaulting to `idle,done,blocked`; an
   unknown spelling is a usage error (exit 2), and `unknown` is not spellable,
   being departure rather than a state to await. A tombstone or withdrawal to
   `unknown` ends the wait as a departure (exit 1); an absent record is refused
   like `prompt` (exit 2); timeout is 124.

6. **`phux agent prompt --wait` is one process on one connection, and that is
   why no sequence counter is needed.** The CLI subscribes, records the
   pre-submit value, then submits; the server writes before replying and pushes
   frames on that connection in order, so every `METADATA_CHANGED` after the
   result was *published* post-write. The screen behind it may be a tick plus
   the idle hold older, a skew a counter shares. Only a post-result transition
   satisfies `prompt --wait`; the pre-submit level never does. **The reasoning
   fails for a caller that submits on one connection and waits on another** —
   `prompt … && wait …`, two MCP calls, a federated path. Subscriptions are
   connection-scoped, so that caller has no shared ordering point and must use
   `prompt --wait`.

7. **`--json` states what the receipt attests**, in one `schema_version: 1`
   document: `delivery` (`acked` / `unknown` / `refused`), `operation_id`, the
   `agent` record the ownership check passed on, `pre_submit_state`,
   `transition_observed`, `matched_by` (`transition` / `level`), `waited_ms`,
   and whether the wait degraded to polling. Errors use ADR-0065's error object.
   The connection must declare `Layer::L3` or the subscribe is dropped silently.

No frame, tag, capability bit, or `phux.agent/v1` field is allocated,
`PROTOCOL_VERSION` does not move, and ADR-0071's frozen surfaces are untouched;
`docs/consumers/agents.md` §2 and §5.2 are owed updates.

## Why

The failure modes worth engineering against are the silent double and the silent
drop, and one shipped mechanism answers both: an operation id with a cached
result. It costs nothing new on the wire and turns the orchestrator's worst
ambiguity into a typed, reportable outcome.

`idle` is read two ways here on purpose. As a *level* it asserts only the
absence of contrary evidence — the right predicate for "do not disturb this
pane", the wrong one for "this pane finished". As an *edge*, `working -> idle`
asserts that whatever claimed `working` stopped claiming it: positive evidence
about a transition even where the level is not positive evidence about a
condition. Safety gates take the level, completion gates take the edge, which is
why point 5 refuses to be satisfied by a read. A new open-enum value
(`quiescent`) was rejected — only a positive idle rule could emit one, and none
can be honestly authored until viewports are captured from the real agent CLIs,
a bill ADR-0046's Tradeoffs already paid once.

## Tradeoffs

An acknowledged submit is slower and occupies the single server-wide
acknowledged lane, so two orchestrators prompting different panes collide into
`RESOURCE_EXHAUSTED` — and that lane's completion wait blocks the one thread
every attached keystroke also flows through, a latent server-wide input hiccup
this verb is the first to expose. Refusing satellite targets makes both verbs
less uniform than `send-keys` until federation carries the guarantee.

A slave that flushes its input queue — `TCSAFLUSH` on a raw-mode toggle, which
every TUI does when it shells out and returns — discards an ACKed batch
silently. The honest recovery is the timeout plus `phux agent explain`, not a
shorter inference window. The ownership bound is likewise real for a detected
record and absent for a declared one, reported after the fact rather than
prevented, because prevention means putting a derived agent judgment in the
input path.

Requiring an edge costs the sub-tick turn: a prompt answered inside one detect
tick derives `idle -> idle`, publishes nothing, and times out at 124 on work
that succeeded. That is the price of never returning 0 on a corpse, and
`transition_observed: false` says which happened.

## Alternatives

**Keep `ROUTE_INPUT` for prompts.** Rejected: the caller cannot distinguish a
lost prompt from a working agent, and its only recovery duplicates it.

**A five-second post-submit activity gate and a stalled error.** Rejected:
inferring delivery from repaint behavior is the output-inference oracle ADR-0053
declined, and it is unsound both ways — a slow agent fails it having received
the prompt, a spinner passes it having dropped one.

**A `state_change_seq` field on `phux.agent/v1`.** Rejected: additive surface on
a record [ADR-0071](./0071-what-phux-1-0-commits-to.md) wants frozen, buying
causality subscription ordering already provides and only moving the
split-connection race rather than closing it.

**A server-side "seen" bit.** Rejected: unseen-ness is per-viewer, so deriving
it server-side would let one client's focus change what another renders — the
authority [ADR-0049](./0049-client-local-focus-and-advisory-attention.md) keeps
client-local.

**Extend `phux wait` with an agent-state condition.** Rejected: `wait` is a
screen-condition poll, this is a subscription with different sources and exit
semantics; the cost is one verb under an existing noun
([ADR-0065](./0065-one-cli-grammar.md)). The shared `--until` spelling carries a
different value domain, which the noun disambiguates.
