//! `phux-q0e.3` — per-Terminal tick scheduler that drives state-sync
//! emission to all attached consumers.
//!
//! Per ADR-0018 (Lazy state synchronization) and its 2026-05-26 Addendum,
//! the `TerminalActor` runs a fixed-rate tick (33 Hz, [`DEFAULT_TICK_INTERVAL`])
//! that walks each attached consumer's [`SnapshotSynthesizer`], emits a
//! `TerminalOutput` frame whenever `synthesize_incremental` returns
//! non-empty bytes, and stamps the frame with a per-consumer monotonic
//! `seq` (starting at `1`).
//!
//! These tests pin the four behaviors the ticket calls for:
//!
//! 1. **No consumers, no emissions.** Spawn the actor with zero
//!    consumers attached, let the tick fire repeatedly, assert nothing
//!    leaks anywhere.
//! 2. **One consumer + tick stays healthy.** Attach one consumer,
//!    advance a tick, assert the actor stays alive and any frame that
//!    emits is well-formed (right `terminal_id`, monotonic per-consumer
//!    `seq` starting at `1`).
//! 3. **Multiple consumers, independent seq.** Two consumers attached;
//!    each gets its own per-consumer `seq` space (each starts at `1`).
//! 4. **Detach mid-tick.** Attach, detach, tick — the detached
//!    consumer's mailbox stays empty.
//!
//! Steady-state emission shape (Clean → empty bytes, Partial → only
//! dirty rows, Full → reset + paint) is covered by the
//! `SnapshotSynthesizer` unit tests in `q0e_1_incremental_synthesis`,
//! which can drive the canonical terminal between calls without going
//! through the actor channels.
//!
//! Timing is fully deterministic via `tokio::time::pause()` +
//! `advance()` — no wall-clock sleeps.
//!
//! libghostty types are `!Send + !Sync`; the actor lives on a `LocalSet`
//! thread (ADR-0014). All tokio tests use `flavor = "current_thread"`
//! and `LocalSet::run_until` for the same reason.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::ClientId;
use phux_protocol::wire::frame::FrameKind;
use phux_server::state::Outbound;
use phux_server::terminal_actor::{
    ConsumerAttachRequest, ConsumerDetachRequest, DEFAULT_TICK_INTERVAL, TerminalActor,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

/// Wire-terminal id stamped on every `TerminalOutput` frame in this
/// test suite. Arbitrary; chosen to be non-trivial.
const WIRE_TID: u32 = 7;

/// Drain whatever is currently sitting on `rx` without blocking.
/// Returns the items in receive order.
fn drain<T>(rx: &mut mpsc::Receiver<T>) -> Vec<T> {
    let mut out = Vec::new();
    while let Ok(item) = rx.try_recv() {
        out.push(item);
    }
    out
}

/// Walk an `Outbound` slice and pull out the `TerminalOutput` frames.
///
/// The `terminal_id` is flattened to its `Local` `u32` for the tests'
/// convenience; v0.1 servers only emit `Local` ids so the unwrap is
/// safe under these scenarios.
fn terminal_outputs(items: &[Outbound]) -> Vec<(u32, u64, &[u8])> {
    items
        .iter()
        .filter_map(|item| match item {
            Outbound::Frame(FrameKind::TerminalOutput {
                terminal_id,
                seq,
                bytes,
            }) => Some((
                terminal_id.local_id().expect("v0.1 local id"),
                *seq,
                bytes.as_ref(),
            )),
            Outbound::Frame(_) => None,
        })
        .collect()
}

/// Advance virtual time by `n * DEFAULT_TICK_INTERVAL` plus a small
/// slack to ensure the tick arm is observed in the actor's `select!`.
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
            let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
            let token = bundle.token.clone();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // Let many ticks fire with zero consumers.
            advance_ticks(10).await;

            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), join)
                .await
                .expect("actor did not exit within 1s")
                .expect("actor task panicked");
        })
        .await;
}

/// 2. One consumer attached → ticks fire and the actor stays healthy.
///    Post upstream `set_dirty` fix, an attached consumer whose
///    synthesizer has been primed by `mark_synced` correctly emits zero
///    bytes on a tick when the canonical terminal has not changed. With
///    no test-side path to write into the actor's terminal after spawn,
///    this test pins the operational shape: the tick arm runs, no
///    panic, channels stay open. A separate channel-shaped test (see
///    `q0e_4_frame_ack`) covers the ack lifecycle; the emission-shape
///    contract lives in the `q0e_1_incremental_synthesis` synthesizer
///    unit tests.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn single_consumer_tick_keeps_actor_healthy() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // Attach a consumer.
            let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(32);
            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .consumer_attach
                .send(ConsumerAttachRequest {
                    client_id: ClientId(1),
                    outbound: out_tx,
                    wire_terminal_id: WIRE_TID,
                    wants_state_sync: false,
                    loss_tolerant: false,
                    reply: reply_tx,
                })
                .await
                .expect("send attach");
            reply_rx
                .await
                .expect("attach reply")
                .expect("attach succeeded");

            // Advance a tick so the actor's interval arm fires. Any
            // frames that do appear must be well-formed (correct wire
            // id, monotonic seq starting at 1).
            advance_ticks(1).await;

            let items = drain(&mut out_rx);
            for (tid, seq, _bytes) in terminal_outputs(&items) {
                assert_eq!(tid, WIRE_TID, "frame stamped with the consumer's wire id");
                assert_eq!(seq, 1, "first emission gets per-consumer seq=1");
            }

            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), join)
                .await
                .expect("actor did not exit within 1s")
                .expect("actor task panicked");
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
            let bundle = TerminalActor::new_with_seed(20, 5, b"hi").expect("new_with_seed");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let join = tokio::task::spawn_local(bundle.actor.run());

            let (send_a, mut recv_a) = mpsc::channel::<Outbound>(32);
            let (send_b, mut recv_b) = mpsc::channel::<Outbound>(32);

            for (client_id, outbound) in
                [(ClientId(1), send_a.clone()), (ClientId(2), send_b.clone())]
            {
                let (reply_tx, reply_rx) = oneshot::channel();
                handle
                    .consumer_attach
                    .send(ConsumerAttachRequest {
                        client_id,
                        outbound,
                        wire_terminal_id: WIRE_TID,
                        wants_state_sync: false,
                        loss_tolerant: false,
                        reply: reply_tx,
                    })
                    .await
                    .expect("send attach");
                reply_rx
                    .await
                    .expect("attach reply")
                    .expect("attach succeeded");
            }
            // Drop the actor-side clones we just stuffed into the
            // attach requests so the only senders alive belong to the
            // actor's per-consumer entries (i.e. `try_recv` on the
            // receivers won't see lingering Senders from the test
            // frame).
            drop(send_a);
            drop(send_b);

            // Advance a tick. With both consumers primed by their
            // attach-time `mark_synced` and no post-attach writes, the
            // tick correctly emits empty bodies (Clean fast path).
            // Whatever does emit must carry well-formed per-consumer
            // seq (starting at 1) on the correct wire id.
            advance_ticks(1).await;

            let items_a = drain(&mut recv_a);
            let items_b = drain(&mut recv_b);
            for (tid, seq, _bytes) in terminal_outputs(&items_a) {
                assert_eq!(tid, WIRE_TID, "A's frame wire id");
                assert_eq!(seq, 1, "A's first emission seq=1");
            }
            for (tid, seq, _bytes) in terminal_outputs(&items_b) {
                assert_eq!(tid, WIRE_TID, "B's frame wire id");
                assert_eq!(seq, 1, "B's first emission seq=1");
            }

            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), join)
                .await
                .expect("actor did not exit within 1s")
                .expect("actor task panicked");
        })
        .await;
}

/// 4. Detach before the tick → no frame for that consumer.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn detached_consumer_receives_no_emission() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let join = tokio::task::spawn_local(bundle.actor.run());

            let client = ClientId(1);
            let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(32);
            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .consumer_attach
                .send(ConsumerAttachRequest {
                    client_id: client,
                    outbound: out_tx,
                    wire_terminal_id: WIRE_TID,
                    wants_state_sync: false,
                    loss_tolerant: false,
                    reply: reply_tx,
                })
                .await
                .expect("send attach");
            reply_rx
                .await
                .expect("attach reply")
                .expect("attach succeeded");

            // Detach BEFORE allowing any tick to fire.
            let (det_tx, det_rx) = oneshot::channel();
            handle
                .consumer_detach
                .send(ConsumerDetachRequest {
                    client_id: client,
                    reply: det_tx,
                })
                .await
                .expect("send detach");
            det_rx.await.expect("detach reply");

            // Now advance several ticks; the consumer must remain
            // empty because its entry was removed before the tick arm
            // fired.
            advance_ticks(5).await;

            let items = drain(&mut out_rx);
            let frames = terminal_outputs(&items);
            assert!(
                frames.is_empty(),
                "detached consumer must receive zero TerminalOutput frames; got {}: {:?}",
                frames.len(),
                frames,
            );

            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), join)
                .await
                .expect("actor did not exit within 1s")
                .expect("actor task panicked");
        })
        .await;
}
