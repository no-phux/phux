---
audience: contributors
stability: evolving
last-reviewed: 2026-10-11
---

# 0166 — Client attention policy lives in the binding crate

**TL;DR.** How urgent a pane is (its attention grade) and the order a pane
list is shown in (triage) are platform-neutral client policy. They are
derived once, in `phux-client-ffi`'s `projection/attention.rs`, as pure
functions over the folded pane status and the agent badge. A JSON fixture
pins the rules, and every mobile client reads them through the `uniffi`
encoder.

Status: Proposed
Date: 2026-10-11

## Context

[ADR-0135](./0135-one-binding-crate.md) made `projection/` the one place
where runtime values become product vocabulary, so the two encoders could
not drift apart. Decision 2 scoped that layer to readings of runtime values.
Everything above it was left to each client.

phux-mobile then wrote the attention rules twice. iOS grades a pane with
`AttentionGrade.of` and orders lists with `triageSorted`. That code puts a
floor of `Normal` under every waiting pane and a ceiling of `Normal` on
every record that does not flag itself, then sorts by band, sub-rank,
activity minute, and input index. Android's `PaneAttention.kt` counts only
`blocked` panes as needing the user and sorts by the raw wire level. It has
already diverged from iOS in the exact case the floor exists for. An ask
hook arrives about 300 ms before the detector's record (ADR-0046), and in
that window a stale `idle` record mutes the pane. This is the drift
ADR-0133 and ADR-0135 were written to prevent, one layer higher.

## Decision

1. **`projection/attention.rs` owns the grade and the triage order.** It
   exposes `grade(PaneStatus, Option<DeclaredAgent>) -> AttentionGrade`,
   `DeclaredAgent::flags_urgency`, `activity_minute`, and
   `triage_order(&[TriagePane]) -> Vec<usize>`. They are pure,
   deterministic, and do no I/O. `flags_urgency` reads the same
   `derived_attention` table that `projection::agent` uses, so a client no
   longer keeps its own copy of that table.
2. **The input is the client's folded status, not a runtime event.**
   `PaneStatus` (`Working`, `Asked`, `Blocked`, `Done { exit }`, `Idle`) is
   the status a card shows. Until a shared status fold exists, each client
   produces it. This is the one way the layer goes beyond ADR-0135's
   "runtime values and nothing else". The grade reads status. A status
   fold never reads a grade.
3. **The semantics are iOS's.** The grade is the declared level (`Normal`
   when there is no record). It is capped at `Normal` unless the record
   flags itself, then floored at `Normal` while the pane waits. The
   triage key is band (waiting, failure, rest), then the grade inside the
   waiting band only, then the activity minute (newest first), then the
   input index. Minutes count from 2001-01-01 UTC and truncate toward zero,
   as Swift's `timeIntervalSinceReferenceDate / 60` does.
4. **One fixture, three readers.** `crates/phux-client-ffi/fixtures/
   attention.json` holds the full grade matrix (6 statuses by 21 records)
   and the triage cases. The Rust tests assert it. iOS and Android read the
   same file at their `PHUX_REV` to test their marshalling.
5. **Encoders stay mechanical.** `uniffi` exports `attention_grade`,
   `agent_flags_urgency`, `triage_order` (returning `u32` indices), and
   `activity_minute`. The C ABI does not export these yet. Cockpit can add
   them when it needs them, from the same layer.

## Why

A rule that decides whether a phone stays silent cannot have one copy per
platform. Android's drift shows what happens when it does. A pure function
over values the client already has is the smallest surface that removes the
copies. Returning indices keeps each client's own row types out of the FFI.

## Tradeoffs

- Swift's reference-date minute is now part of the cross-platform contract.
  It matches Unix minutes for every time after 2001, so the only cost is
  one constant.
- `flags_urgency` still compares the resolved level with the derived one.
  A record that says `blocked` with `high` reads as unflagged, the same as
  on iOS. `AttentionReading::Declared` could tell the two apart, but that
  would be a behaviour change, so it is left for a later decision.
- Presentation stays on each platform: settings labels, VoiceOver phrases,
  haptics, persisted-threshold clamping, and the generic attention float.

## Alternatives

**A Kotlin port next to the Swift one.** Rejected by the Android parity
roadmap: two copies with a shared fixture still means two implementations
to keep in step.

**Grade inside `projection::agent`'s badge.** Rejected because the grade
depends on the folded status, which the badge does not know. Putting it
there would turn the one-way dependency in decision 2 into a cycle.
