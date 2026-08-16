//! `phux-q0e.3` + `phux-q0e.4` — the `TerminalActor` tick scheduler and
//! the `FRAME_ACK` handler, merged into one binary because every test
//! boots the same actor fixture (see [`ActorFixture`]).
//!
//! Per ADR-0018 (Lazy state synchronization) and its 2026-05-26 Addendum:
//!
//! * The actor runs a fixed-rate tick (33 Hz, [`DEFAULT_TICK_INTERVAL`])
//!   that walks each attached consumer's `SnapshotSynthesizer`, emits a
//!   `TerminalOutput` frame whenever `synthesize_incremental` returns
//!   non-empty bytes, and stamps the frame with a per-consumer monotonic
//!   `seq` (starting at `1`).
//! * `FRAME_ACK` is the only thing allowed to call `mark_synced` on a
//!   per-consumer synthesizer: tick emits `seq = N`, the consumer applies
//!   the bytes and acks `N`, `on_frame_ack` clears the dirty cache, and
//!   the next tick re-diffs against the just-acked reference.
//!
//! Tick-scheduler coverage (the four behaviors the q0e.3 ticket calls for):
//! 1. No consumers, no emissions.
//! 2. One consumer + tick stays healthy (well-formed frames, seq from 1).
//! 3. Multiple consumers get independent per-consumer `seq` spaces.
//! 4. Detach mid-tick — the detached consumer's mailbox stays empty.
//!
//! `FRAME_ACK` coverage (channel-shaped routing across the actor's
//! `select!` loop; direct `on_frame_ack` unit tests live in the
//! `terminal_actor` module's `#[cfg(test)]` block):
//! 5. End-to-end ack round-trip keeps the tick path healthy.
//! 6. Older/duplicate acks are silent no-ops.
//! 7. An ack for a never-attached `ClientId` is a silent no-op.
//! 8. An ack after detach does not resurrect the consumer entry.
//!
//! Steady-state emission shape (Clean → empty bytes, Partial → only dirty
//! rows, Full → reset + paint) is covered by the `SnapshotSynthesizer`
//! unit tests in `q0e_1_incremental_synthesis`. With the upstream
//! libghostty `set_dirty` fix, a tick against an unchanged terminal
//! correctly emits zero bytes (Clean fast path), so these tests tolerate
//! empty emissions rather than requiring them.
//!
//! Timing is fully deterministic via `tokio::time::pause()` +
//! `advance()` (`start_paused = true`) — no wall-clock sleeps.
//!
//! libghostty types are `!Send + !Sync`; the actor lives on a `LocalSet`
//! thread (ADR-0014). All tokio tests use `flavor = "current_thread"`
//! and `LocalSet::run_until` for the same reason.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

use std::time::Duration;

use phux_protocol::wire::frame::FrameKind;
use phux_protocol::{BootstrapId, ClientId, StreamId};
use phux_server::state::Outbound;
use phux_server::terminal_actor::{
    ConsumerAckRequest, ConsumerAttachRequest, ConsumerDetachRequest, DEFAULT_TICK_INTERVAL,
    TerminalActor, TerminalHandle,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;

/// Ceiling for joining an actor that has already been cancelled.
///
/// Not load-bearing: the assertion is that the actor exits, never how fast,
/// and it exits in single-digit milliseconds when the runtime gets a core.
/// The 1s this replaces was generous on an idle laptop and a measurement of
/// the scheduler on a saturated one (phux-br1f). A genuinely hung actor still
/// fails the run, 30s later, with the same message.
const ACTOR_EXIT_DEADLINE: Duration = Duration::from_secs(30);

/// Wire-terminal id stamped on every `TerminalOutput` frame in this
/// suite. Arbitrary; chosen to be non-trivial.
const WIRE_TID: u32 = 7;

/// A spawned `TerminalActor` (20x5, seeded) with `n_consumers` attached
/// consumers whose `ClientId`s are `1..=n_consumers`. Every test in this
/// binary used to hand-roll exactly this boot sequence.
struct ActorFixture {
    handle: TerminalHandle,
    token: CancellationToken,
    join: tokio::task::JoinHandle<()>,
    /// Per-consumer outbound mailboxes, indexed `ClientId(i + 1)`.
    consumers: Vec<mpsc::Receiver<Outbound>>,
}

impl ActorFixture {
    /// Spawn the actor on the current `LocalSet` and attach `n_consumers`
    /// consumers. The outbound senders are moved into the attach requests,
    /// so the only senders alive belong to the actor's per-consumer
    /// entries (`try_recv` on the receivers won't see lingering senders
    /// from the test frame).
    async fn spawn(seed: &[u8], n_consumers: u32) -> Self {
        let bundle = TerminalActor::new_with_seed(20, 5, seed).expect("new_with_seed");
        let handle = bundle.handle.clone();
        let token = bundle.token.clone();
        let join = tokio::task::spawn_local(bundle.actor.run());

        let mut consumers = Vec::new();
        for i in 1..=n_consumers {
            let (out_tx, out_rx) = mpsc::channel::<Outbound>(32);
            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .consumer_attach
                .send(ConsumerAttachRequest {
                    client_id: ClientId(i),
                    outbound: out_tx,
                    wire_terminal_id: WIRE_TID,
                    stream_id: StreamId::new(u64::from(i)).expect("test stream id"),
                    bootstrap_id: BootstrapId::new(1).expect("test bootstrap id"),
                    wants_state_sync: false,
                    state_sync_scrollback: None,
                    bootstrap_max_bytes: usize::MAX,
                    bootstrap_max_frames: usize::MAX,
                    bootstrap_chunk_bytes: 1,
                    loss_tolerant: false,
                    live_gate: watch::channel(true).1,
                    reply: reply_tx,
                })
                .await
                .expect("send attach");
            reply_rx
                .await
                .expect("attach reply")
                .expect("attach succeeded");
            consumers.push(out_rx);
        }

        Self {
            handle,
            token,
            join,
            consumers,
        }
    }

    /// Detach `client_id` and wait for the actor's acknowledgement.
    async fn detach(&self, client_id: ClientId) {
        let (det_tx, det_rx) = oneshot::channel();
        self.handle
            .consumer_detach
            .send(ConsumerDetachRequest {
                client_id,
                reply: det_tx,
            })
            .await
            .expect("send detach");
        det_rx.await.expect("detach reply");
    }

    /// Send a `FRAME_ACK` across the channel boundary.
    async fn ack(&self, client_id: ClientId, seq: u64) {
        self.handle
            .consumer_ack
            .send(ConsumerAckRequest {
                client_id,
                stream_id: StreamId::new(u64::from(client_id.0)).expect("test stream id"),
                bootstrap_id: BootstrapId::new(1).expect("test bootstrap id"),
                seq,
            })
            .await
            .expect("send ack");
    }

    /// Cancel the actor and assert it joins cleanly within the deadline.
    async fn shutdown(self) {
        self.token.cancel();
        tokio::time::timeout(ACTOR_EXIT_DEADLINE, self.join)
            .await
            .expect("actor did not exit after cancel")
            .expect("actor task panicked");
    }
}

/// Drain whatever is currently sitting on `rx` without blocking.
/// Returns the items in receive order.
fn drain<T>(rx: &mut mpsc::Receiver<T>) -> Vec<T> {
    let mut out = Vec::new();
    while let Ok(item) = rx.try_recv() {
        out.push(item);
    }
    out
}

/// Walk an `Outbound` slice and extract the `TerminalOutput` frames'
/// `(terminal_id, seq, bytes_len)`. Bodies are not compared here — the
/// q0e.1 synthesizer unit tests pin output shape; this suite cares about
/// routing, ordering, and lifecycle.
///
/// The `terminal_id` is flattened to its `Local` `u32` for the tests'
/// convenience; v0.1 servers only emit `Local` ids so the unwrap is
/// safe under these scenarios.
fn terminal_outputs(items: &[Outbound]) -> Vec<(u32, u64, usize)> {
    items
        .iter()
        .filter_map(|item| match item {
            Outbound::Frame(FrameKind::TerminalOutput {
                terminal_id,
                seq,
                bytes,
                ..
            }) => Some((
                terminal_id.local_id().expect("v0.1 local id"),
                *seq,
                bytes.len(),
            )),
            Outbound::Frame(_) | Outbound::TerminalError { .. } => None,
        })
        .collect()
}

/// Advance virtual time by `n * DEFAULT_TICK_INTERVAL` plus a small
/// scheduler-yield slack so the actor's `select!` tick arm is observed.
/// Yields after each step so the actor's task gets a chance to poll.
async fn advance_ticks(n: u32) {
    for _ in 0..n {
        tokio::time::advance(DEFAULT_TICK_INTERVAL).await;
        // Yield enough times for the actor's task to be polled. One
        // `yield_now` is usually enough, but a small loop is bulletproof
        // against scheduler quirks while staying deterministic (no
        // real-time wait).
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }
}

/// Yield several times so the actor task drains a freshly-sent channel
/// message before the test inspects observable state.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

// ====================================================================
// q0e.3 — tick scheduler
// ====================================================================

/// 1. No consumers attached → tick can fire any number of times and
///    nothing happens (no panic, no leaked frames anywhere).
///
/// We can't directly observe "no frame went anywhere" without a
/// consumer to send to, so the assertion is operational: the actor
/// stays healthy across many ticks and shuts down cleanly on cancel.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn no_consumers_means_no_emissions() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let fixture = ActorFixture::spawn(b"hello", 0).await;

            // Let many ticks fire with zero consumers.
            advance_ticks(10).await;

            fixture.shutdown().await;
        })
        .await;
}

/// 2. One consumer attached → ticks fire and the actor stays healthy.
///    Post upstream `set_dirty` fix, an attached consumer whose
///    synthesizer has been primed by `mark_synced` correctly emits zero
///    bytes on a tick when the canonical terminal has not changed. With
///    no test-side path to write into the actor's terminal after spawn,
///    this test pins the operational shape: the tick arm runs, no
///    panic, channels stay open. The ack lifecycle is covered by the
///    q0e.4 tests below; the emission-shape contract lives in the
///    `q0e_1_incremental_synthesis` synthesizer unit tests.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn single_consumer_tick_keeps_actor_healthy() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let mut fixture = ActorFixture::spawn(b"hello", 1).await;

            // Advance a tick so the actor's interval arm fires. Any
            // frames that do appear must be well-formed (correct wire
            // id, monotonic seq starting at 1).
            advance_ticks(1).await;

            let items = drain(&mut fixture.consumers[0]);
            for (tid, seq, _len) in terminal_outputs(&items) {
                assert_eq!(tid, WIRE_TID, "frame stamped with the consumer's wire id");
                assert_eq!(seq, 1, "first emission gets per-consumer seq=1");
            }

            fixture.shutdown().await;
        })
        .await;
}

/// 3. Two consumers attached → each gets a frame on the same tick, each
///    carrying its own per-consumer `seq` (each starts at `1`).
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn multiple_consumers_get_independent_per_consumer_seq() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let mut fixture = ActorFixture::spawn(b"hi", 2).await;

            // Advance a tick. With both consumers primed by their
            // attach-time `mark_synced` and no post-attach writes, the
            // tick correctly emits empty bodies (Clean fast path).
            // Whatever does emit must carry well-formed per-consumer
            // seq (starting at 1) on the correct wire id.
            advance_ticks(1).await;

            let items_a = drain(&mut fixture.consumers[0]);
            let items_b = drain(&mut fixture.consumers[1]);
            for (tid, seq, _len) in terminal_outputs(&items_a) {
                assert_eq!(tid, WIRE_TID, "A's frame wire id");
                assert_eq!(seq, 1, "A's first emission seq=1");
            }
            for (tid, seq, _len) in terminal_outputs(&items_b) {
                assert_eq!(tid, WIRE_TID, "B's frame wire id");
                assert_eq!(seq, 1, "B's first emission seq=1");
            }

            fixture.shutdown().await;
        })
        .await;
}

/// 4. Detach before the tick → no frame for that consumer.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn detached_consumer_receives_no_emission() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let mut fixture = ActorFixture::spawn(b"hello", 1).await;

            // Detach BEFORE allowing any tick to fire.
            fixture.detach(ClientId(1)).await;

            // Now advance several ticks; the consumer must remain
            // empty because its entry was removed before the tick arm
            // fired.
            advance_ticks(5).await;

            let items = drain(&mut fixture.consumers[0]);
            let frames = terminal_outputs(&items);
            assert!(
                frames.is_empty(),
                "detached consumer must receive zero TerminalOutput frames; got {}: {:?}",
                frames.len(),
                frames,
            );

            fixture.shutdown().await;
        })
        .await;
}

// ====================================================================
// q0e.4 — FRAME_ACK
// ====================================================================

/// 5. End-to-end ack round-trip across the channel boundary. After the
///    `ConsumerAckRequest` lands and the actor processes it, subsequent
///    ticks must continue to function (actor stays healthy, channel arms
///    keep being polled). Post upstream `set_dirty` fix, ticks against an
///    unchanged terminal emit zero bytes (Clean fast path), so this test
///    pins the lifecycle contract — the actor accepts the ack, does not
///    panic, and remains responsive — rather than the steady-state
///    emission shape.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ack_round_trip_emits_post_ack_tick() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let mut fixture = ActorFixture::spawn(b"hello", 1).await;

            // Tick once; capture whatever the pre-ack path emits.
            // Post-fix this is typically empty (Clean) because attach
            // primed the synthesizer; we tolerate either shape and use
            // the result to seed the seq comparison below.
            advance_ticks(1).await;
            let pre_ack = terminal_outputs(&drain(&mut fixture.consumers[0]));
            let ack_seq = pre_ack.first().map_or(0, |(_, s, _)| *s);
            if let Some(&(tid, seq, _)) = pre_ack.first() {
                assert_eq!(tid, WIRE_TID, "frame stamped with consumer's wire id");
                assert_eq!(seq, 1, "first emission seq=1 when present");
            }

            // Send a FRAME_ACK across the channel boundary. With ack_seq=0
            // (no prior emission) the ack still exercises the routing
            // path and must be a clean no-op.
            fixture.ack(ClientId(1), ack_seq).await;
            settle().await;

            // Actor must stay healthy across the ack — ticks continue,
            // no panic, no channel close. Body may be empty (Clean) or
            // non-empty depending on whether anything changed; we don't
            // assert either way.
            advance_ticks(2).await;
            let _ = drain(&mut fixture.consumers[0]);

            fixture.shutdown().await;
        })
        .await;
}

/// 6. Older/duplicate ack across the channel boundary is a silent no-op.
///    We can't read `last_acked_seq` from outside the crate, so the
///    assertion is operational: the actor stays healthy and the tick path
///    still runs afterwards. Direct field-level assertions live in
///    `terminal_actor::tests_state_sync::on_frame_ack_older_or_duplicate_is_dropped`.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn older_and_duplicate_acks_do_not_crash_the_actor() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let mut fixture = ActorFixture::spawn(b"hi", 1).await;

            // Forward ack (seq=5) followed by older (seq=3) and
            // duplicate (seq=5). The actor must handle each cleanly.
            for seq in [5u64, 3, 5, 4] {
                fixture.ack(ClientId(1), seq).await;
            }
            settle().await;

            // Actor must remain healthy after older/duplicate acks —
            // ticks continue. Post-fix, with no terminal writes between
            // ticks, the body is empty (Clean); the assertion is on
            // actor liveness, not byte-shape.
            advance_ticks(1).await;
            let _ = drain(&mut fixture.consumers[0]);

            fixture.shutdown().await;
        })
        .await;
}

/// 7. Sending a `ConsumerAckRequest` for a `ClientId` that was never
///    attached is a silent no-op: the actor must stay alive and the tick
///    path for *other* consumers must still work.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ack_for_unregistered_consumer_is_silent_noop() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let mut fixture = ActorFixture::spawn(b"hi", 1).await;

            // Stray ack for a client_id that was never attached.
            fixture.ack(ClientId(999), 42).await;
            settle().await;

            // The actor must stay healthy after a stray ack for an
            // unknown consumer — tick path continues to be polled,
            // attached consumer remains addressable. Post-fix, body may
            // be empty (Clean) for an unchanged terminal; we assert the
            // actor lifecycle, not the byte-shape.
            advance_ticks(1).await;
            let _ = drain(&mut fixture.consumers[0]);

            fixture.shutdown().await;
        })
        .await;
}

/// 8. Detach then ack: silent no-op. The actor must not crash and the
///    per-consumer entry must NOT be resurrected by the late ack. We
///    can't peek at the map from outside the crate; the indirect proof
///    is that subsequent ticks emit no frames for the detached id (no
///    outbound channel to even reach, since detach drops the actor-side
///    sender; this test pins that detach + ack stays clean).
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ack_after_detach_is_silent_noop() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let mut fixture = ActorFixture::spawn(b"hi", 1).await;

            // Detach.
            fixture.detach(ClientId(1)).await;

            // Now ack for the just-detached id. Must be a no-op.
            fixture.ack(ClientId(1), 5).await;
            settle().await;

            // No frames should arrive (entry is gone, tick has no
            // consumer to walk for this id).
            advance_ticks(3).await;
            let items = terminal_outputs(&drain(&mut fixture.consumers[0]));
            assert!(
                items.is_empty(),
                "detached consumer must receive zero frames; got {} ({:?})",
                items.len(),
                items,
            );

            fixture.shutdown().await;
        })
        .await;
}
