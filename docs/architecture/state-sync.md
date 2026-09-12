---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# State synchronization

**TL;DR.** How an attaching client catches up to a resource that already
has output. Each Terminal stream negotiates one of three bootstrap
profiles: an exact libghostty checkpoint followed by raw PTY bytes (the
native path), or a synthesized-VT snapshot followed by either raw bytes or
per-consumer StateSync diffs. Live bytes are the same stream every
subscriber sees; only the synthesized StateSync profile diffs per consumer.

---

## Per-kind codecs

A resource's output stream carries opaque bytes under a codec negotiated
per stream; the bootstrap is a replaceable replica generation cut at a
stream sequence (`BootstrapId`, ADR-0070). The Terminal kind has three
profiles, selected once per connection at HELLO and repeated per stream in
`BOOTSTRAP_BEGIN`:

| Profile | Bootstrap codec | Live `RESOURCE_OUTPUT.bytes` | `FRAME_ACK` | Color rewrite |
|---|---|---|---|---|
| `NativeState` | libghostty checkpoint (`BootstrapCodec::Native`) | raw PTY bytes | forbidden | forbidden |
| `SynthesizedVtRaw` | `SynthesizedVtV1` | raw compatibility VT | forbidden | per client caps |
| `SynthesizedVtStateSync` | `SynthesizedVtV1` | per-consumer diff | cumulative | per client caps |

`BootstrapProfile` is an enum of these three combinations, so
`NativeState` plus StateSync is unrepresentable. Native is preferred when
both ends advertise a compatible engine codec; otherwise a synthesized
profile is selected only if both advertised that exact combination, and no
shared profile is a fatal `CODEC_UNAVAILABLE` at HELLO
([ADR-0070](../../ADR/0070-native-engine-state-bootstrap.md)).

An AgentSession stream uses `AgentEventsJsonlV1` only: bootstrap is the
retained JSONL ring, live `RESOURCE_OUTPUT.bytes` are complete records,
raw-only, no `FRAME_ACK`. Profile negotiation does not constrain it.

## Native bootstrap (built, preferred)

Both ends run libghostty and the wire carries engine bytes rather than a
re-encoded grid ([ADR-0013](../../ADR/0013-libghostty-bytes-on-wire.md)).
Under `NativeState` the attach path is:

1. The runtime drains the subscription receiver and asks the Terminal
   engine for an inclusive cut. The engine applies every PTY byte through
   `base_seq`, increments the replica generation, captures the
   authoritative `(cols, rows)`, and starts an immutable codec capture.
2. The server sends `BOOTSTRAP_BEGIN`, contiguous `BOOTSTRAP_CHUNK`s, and
   `BOOTSTRAP_READY` once every engine byte through the engine's READY
   record is out. phux never scans the checkpoint; ghostty produces and
   consumes it (`phux-server::native_state`, `resource::terminal::native`).
3. The client (`phux-client-core::session`) decodes into a staging replica
   and publishes atomically on `BOOTSTRAP_READY`; the next frame is raw
   `RESOURCE_OUTPUT { seq: base_seq + 1 }`. There is no bootstrap ACK.
4. Retained history is client-pull afterward (`HISTORY_REQUEST` /
   `HISTORY_PAGE`, newest to oldest, one outstanding request per stream)
   and never mutates the live screen.

An authoritative resize, a sequence gap, or a bounded-queue overflow takes
a fresh cut and tombstones the old generation (`BOOTSTRAP_TOMBSTONE`); the
client keeps its last published replica until the replacement reaches
READY. Because live bytes are the PTY's own bytes, every subscriber of a
Terminal sees one broadcast stream and no per-consumer state exists for
this profile.

## Synthesized bootstrap (built, compatibility)

The synthesized profiles keep the ADR-0018 shape for consumers without a
compatible engine codec:

1. The engine's `SnapshotSynthesizer` (`phux-server::grid`) reads the
   current `RenderState` and renders a byte sequence that, replayed into a
   fresh engine, reproduces the visible state; it ships as the bootstrap
   under `SynthesizedVtV1`.
2. Under `SynthesizedVtRaw` the client then follows the same raw byte
   stream as native consumers, after the per-client capability rewrite in
   `downsample.rs`.
3. Under `SynthesizedVtStateSync` the engine allocates a per-consumer
   reference grid (`grid::ConsumerReference`) at attach, and its state-sync
   tick emits the minimum VT to move that reference to the current grid
   (`synthesize_against_reference`). The reference advances on emit, so a
   change is delivered exactly once and an idle Terminal emits nothing;
   `FRAME_ACK` advances the consumer's acked sequence for backpressure
   accounting. The reference is a rendered-row copy, not libghostty's dirty
   bits, because `RenderState::update` consumes the shared dirty state and
   would starve every consumer but the first
   ([ADR-0018](../../ADR/0018-lazy-state-synchronization.md) addenda).

Replaying a synthesized snapshot through the same engine that produced it
yields the server's grid up to the documented downsampling rewrites; that
equivalence is a property test, not an assumption
([`verification.md`](./verification.md)).

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Loss-tolerant re-diff against an older reference on a lossy transport | Every shipped transport is reliable and ordered; the reference advances on emit and no re-diff path is wired. | [ADR-0018](../../ADR/0018-lazy-state-synchronization.md) | not scheduled; revisit with a datagram lane |
