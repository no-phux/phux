---
audience: contributors
stability: stable
last-reviewed: 2026-07-11
---

# 0043 — State-diff output mode and loss-tolerant reference advance

**TL;DR.** The negotiated `OutputMode::StateSync` emitter (ADR-0018 made
concrete) synthesizes the minimum-VT transition per consumer per tick,
RTT-paced so runaway output bounds the client's re-parse *rate* rather than
streaming every frame. On a lossy/forwarded leg a consumer opts into an
advance-on-ack reference: the per-consumer reference advances on `FRAME_ACK`,
not on emit, so a dropped frame re-diffs against the last-acked reference and
self-heals. No wire-byte change — both are server-side strategies behind the
existing HELLO negotiation.

Status: Accepted
Date: 2026-07-11

## Context

[ADR-0018](./0018-lazy-state-synchronization.md) named phux's wire as lazy
state synchronization and [ADR-0013](./0013-libghostty-bytes-on-wire.md)'s
pass-through PTY bytes as its degenerate case. Its 2026-05-30 addendum
landed the live machinery: a negotiated `OutputMode::StateSync` (phux-fseo),
a per-consumer reference grid diffed by
`SnapshotSynthesizer::synthesize_against_reference` (phux-ia4), an
RTT-adaptive tick scheduler (phux-q0e.5), and `FRAME_ACK` accounting
(phux-q0e.4) — all on an **emit-once** model: the reference advances on emit
because v0.1 transports (UDS, SSH stdio, WebSocket, QUIC reliable stream) are
reliable and ordered.

Two gaps remained. (a) Forwarding raw VT (ADR-0013) re-parses every wasteful
app repaint byte-for-byte on the client; a state-diff emitter should bound
that under runaway output (phux-51n6.3). (b) That addendum flagged the
loss-tolerance "re-diff against an older reference on a dropped frame"
property as inherent to the reference-grid model but *only wired when a lossy
transport lands*. Federation forwarding now exists: the hub relay
([relay.rs](../../crates/phux-server/src/hub/relay.rs)) fans return-leg
`TERMINAL_OUTPUT` to consumers with `try_send`, **dropping whole frames** when
a consumer mailbox is briefly full. Under emit-once a dropped delta is never
re-sent — the satellite advanced its reference — so the mirror diverges
forever (phux-v45.8).

## Decision

**State-diff output mode (phux-51n6.3).** `OutputMode::StateSync` stays the
negotiated, per-connection opt-in; `Raw` remains the byte-faithful human
default (ADR-0013 degenerate case, byte-identical). The emitter synthesizes
the minimum-VT transition from each consumer's reference to the live grid,
once per tick, RTT-paced (`srtt/2`, clamped 20–200 ms). Coalescence is
structural: N intermediate repaints between two ticks collapse to one delta
sized by the visible change, so the client's re-parse/re-render rate is
bounded by the tick cadence, not the app's output rate. A `StateSync`
consumer and a `Raw` consumer viewing the same terminal converge to
byte-identical grids.

**Loss-tolerant reference advance (phux-v45.8).** A consumer on a
lossy/forwarded leg opts into `loss_tolerant`. Its per-consumer reference
splits into a **last-acked reference** (the diff base) plus **pending
snapshots** keyed by emitted `seq`. Each tick re-diffs the live grid against
the *acked* reference — an absolute, cumulative, per-row-repainting delta —
and does not advance it. `FRAME_ACK` advances the acked reference to the grid
snapshot the cumulative ack covers and prunes older pending snapshots. An
un-acked frame is retransmitted after a retransmit timeout (`3·srtt`, clamped
100 ms–1 s) so a lost final frame on an idle terminal still heals. A dropped
frame therefore self-heals: its rows still differ from the acked reference,
so the next emission re-includes them.

## Why

Per-row deltas are **absolute** (`CUP` + SGR reset + full row body), so
re-diffing from the acked reference and re-applying is idempotent regardless
of which intermediate deltas a consumer received — the property that lets a
dropped frame heal without a retransmit-and-reorder layer. Advancing on ack
rather than emit is the one change that makes "assume delivery" false, which
is exactly what a lossy leg requires. Keeping it opt-in preserves the
cheapest-correct emit-once path for reliable transports (advancing on ack
there would re-ship a cumulative delta every tick and grow unbounded pending
state for a non-acking consumer). No wire change is needed: `FRAME_ACK` and
`seq` already round-trip; the meaning of `TERMINAL_OUTPUT.bytes` under
state-sync (ADR-0018) already permits synthesized diffs.

## Tradeoffs

- **Residual correctness bound.** Without a wire `base_seq`, a row that flips
  A→B→A across a *delivered-but-unacked* frame within one RTT can be missed
  (the acked→live diff sees A→A). The dominant loss mode — a frame the relay
  never delivered — heals exactly, because the consumer stays at the acked
  state. Closing the residual needs the wire to carry the delta's base and
  the client to apply only on a base match (the Mosh fragment/datagram
  layer) — deferred.
- **Memory.** A pending grid snapshot per in-flight `seq` (bounded by the
  ack window, hard-capped like `emit_instants`). Only for loss-tolerant
  consumers.
- **Bandwidth under sustained loss.** Cumulative re-diffs re-ship changed
  rows until an ack lands; the RTT-adaptive cadence and `3·srtt` retransmit
  bound the re-spend.
- **Auto-activation is deferred.** The mechanism is wired end-to-end
  (`ConsumerAttachRequest.loss_tolerant`) but production attach sets it
  `false`: a satellite cannot see the hub's downstream fan-out drop from its
  own reliable link. Flipping it on for forwarded consumers (hub-requested
  loss-tolerant state-sync, or a QUIC-datagram transport signal) is a
  follow-up.

## Alternatives

**Stay emit-once everywhere.** Simplest, correct on reliable transports,
diverges permanently on the forwarded leg's frame drops. Rejected — that is
the bug v45.8 exists to fix.

**Advance-on-ack for all state-sync consumers.** Uniform, but re-ships a
cumulative delta every tick while awaiting an ack and grows pending state for
a non-acking consumer — wasteful on the reliable path that dominates.
Rejected in favour of the opt-in split.

**Port Mosh's SSP wire (state + base_seq + fragment layer).** Fully closes
the residual bound, but libghostty has no `set_state` path (ADR-0013), so the
receiver must apply bytes via `vt_write`. The framework is Mosh's; the wire
is not. Deferred to a wire-versioned follow-up rather than taken here.
