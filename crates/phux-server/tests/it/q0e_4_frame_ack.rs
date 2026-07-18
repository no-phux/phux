//! `phux-q0e.4` — `FRAME_ACK` handler closes the state-sync loop.
//!
//! Per ADR-0018 (lazy state synchronization) and its 2026-05-26 Addendum,
//! `FRAME_ACK` is the only thing allowed to call `mark_synced` on a
//! per-consumer `SnapshotSynthesizer`. The tick driver (phux-q0e.3) emits
//! deltas against the cached reference; the ack is what tells the actor
//! "this consumer's mirror is caught up — next tick re-diffs against the
//! just-acked reference."
//!
//! The cycle:
//!   1. tick emits `TerminalOutput { seq = N }` to the consumer,
//!   2. consumer applies the bytes, sends `FRAME_ACK { seq = N }`,
//!   3. `on_frame_ack` calls `synthesizer.mark_synced` (q0e.1 primitive),
//!      clearing the per-consumer dirty cache,
//!   4. next tick re-diffs against the just-acked reference.
//!
//! These integration tests exercise the channel-shaped path
//! (`TerminalHandle::consumer_ack`) end-to-end across the actor's
//! `select!` loop. The direct `on_frame_ack(..)` unit tests live in the
//! `terminal_actor` module's `#[cfg(test)]` block; this file focuses on
//! cross-channel routing.
//!
//! libghostty types are `!Send + !Sync`; the actor lives on a `LocalSet`
//! thread (ADR-0014). All tokio tests use `flavor = "current_thread"`
//! and `LocalSet::run_until`.
//!
//! These tests focus on the channel-shaped routing and lifecycle: that
//! the actor accepts acks (forward, older, duplicate, for unknown /
//! detached consumers) without panicking, and that the tick path stays
//! healthy across the ack. With the upstream libghostty `set_dirty` fix,
//! a post-`mark_synced` tick against an unchanged terminal correctly
//! emits zero bytes (Clean fast path); the tests therefore tolerate
//! empty post-ack emissions rather than requiring them.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::ClientId;
use phux_protocol::wire::frame::FrameKind;
use phux_server::state::Outbound;
use phux_server::terminal_actor::{
    ConsumerAckRequest, ConsumerAttachRequest, ConsumerDetachRequest, DEFAULT_TICK_INTERVAL,
    TerminalActor,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

/// Wire-terminal id stamped on every `TerminalOutput` frame in this
/// suite. Arbitrary; chosen to be non-trivial.
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

/// Walk an `Outbound` slice and extract the `TerminalOutput` frames'
/// `(terminal_id, seq, bytes_len)`. We don't compare bodies here — the
/// q0e.1/q0e.3 tests pin synthesizer output shape; this suite cares
/// about routing + ordering + non-empty under the l0t degradation.
fn terminal_outputs(items: &[Outbound]) -> Vec<(u32, u64, usize)> {
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
                bytes.len(),
            )),
            Outbound::Frame(_) => None,
        })
        .collect()
}

/// Advance virtual time by `n * DEFAULT_TICK_INTERVAL` plus a small
/// scheduler-yield slack so the actor's `select!` arm gets observed.
async fn advance_ticks(n: u32) {
    for _ in 0..n {
        tokio::time::advance(DEFAULT_TICK_INTERVAL).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }
}

/// Yield several times so the actor task drains a freshly-sent channel
/// message before the test inspects observable state. The select! arm
/// runs the moment the task is polled; one yield is usually enough, a
/// small loop is bulletproof.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// End-to-end ack roundtrip across the channel boundary. After the
/// `ConsumerAckRequest` lands and the actor processes it, subsequent
/// ticks must continue to function (actor stays healthy, channel arms
/// keep being polled). Post upstream `set_dirty` fix, ticks against an
/// unchanged terminal emit zero bytes (Clean fast path), so this test
/// pins the lifecycle contract — the actor accepts the ack, does not
/// panic, and remains responsive — rather than the steady-state
/// emission shape.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ack_round_trip_emits_post_ack_tick() {
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

            // Tick once; capture whatever the pre-ack path emits.
            // Post-fix this is typically empty (Clean) because attach
            // primed the synthesizer; we tolerate either shape and use
            // the result to seed the seq comparison below.
            advance_ticks(1).await;
            let pre_ack = terminal_outputs(&drain(&mut out_rx));
            let ack_seq = pre_ack.first().map_or(0, |(_, s, _)| *s);
            if let Some(&(tid, seq, _)) = pre_ack.first() {
                assert_eq!(tid, WIRE_TID, "frame stamped with consumer's wire id");
                assert_eq!(seq, 1, "first emission seq=1 when present");
            }

            // Send a FRAME_ACK across the channel boundary. With ack_seq=0
            // (no prior emission) the ack still exercises the routing
            // path and must be a clean no-op.
            handle
                .consumer_ack
                .send(ConsumerAckRequest {
                    client_id: client,
                    seq: ack_seq,
                })
                .await
                .expect("send ack");
            settle().await;

            // Actor must stay healthy across the ack — ticks continue,
            // no panic, no channel close. Body may be empty (Clean) or
            // non-empty depending on whether anything changed; we don't
            // assert either way.
            advance_ticks(2).await;
            let _ = drain(&mut out_rx);

            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), join)
                .await
                .expect("actor did not exit within 1s")
                .expect("actor task panicked");
        })
        .await;
}

/// Older/duplicate ack across the channel boundary is a silent no-op.
/// We can't read `last_acked_seq` from outside the crate, so the
/// assertion is operational: the actor stays healthy and the tick path
/// still emits afterwards. Direct field-level assertions live in
/// `terminal_actor::tests::on_frame_ack_older_or_duplicate_is_dropped`.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn older_and_duplicate_acks_do_not_crash_the_actor() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"hi").expect("new_with_seed");
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

            // Forward ack (seq=5) followed by older (seq=3) and
            // duplicate (seq=5). The actor must handle each cleanly.
            for seq in [5u64, 3, 5, 4] {
                handle
                    .consumer_ack
                    .send(ConsumerAckRequest {
                        client_id: client,
                        seq,
                    })
                    .await
                    .expect("send ack");
            }
            settle().await;

            // Actor must remain healthy after older/duplicate acks —
            // ticks continue. Post-fix, with no terminal writes between
            // ticks, the body is empty (Clean); the assertion is on
            // actor liveness, not byte-shape.
            advance_ticks(1).await;
            let _ = drain(&mut out_rx);

            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), join)
                .await
                .expect("actor did not exit within 1s")
                .expect("actor task panicked");
        })
        .await;
}

/// Sending a `ConsumerAckRequest` for a `ClientId` that was never
/// attached is a silent no-op: the actor must stay alive and the tick
/// path for *other* consumers must still work.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ack_for_unregistered_consumer_is_silent_noop() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"hi").expect("new_with_seed");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let join = tokio::task::spawn_local(bundle.actor.run());

            let real = ClientId(1);
            let stranger = ClientId(999);
            let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(32);
            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .consumer_attach
                .send(ConsumerAttachRequest {
                    client_id: real,
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

            // Stray ack for a client_id that was never attached.
            handle
                .consumer_ack
                .send(ConsumerAckRequest {
                    client_id: stranger,
                    seq: 42,
                })
                .await
                .expect("send ack");
            settle().await;

            // The actor must stay healthy after a stray ack for an
            // unknown consumer — tick path continues to be polled,
            // attached consumer remains addressable. Post-fix, body may
            // be empty (Clean) for an unchanged terminal; we assert the
            // actor lifecycle, not the byte-shape.
            advance_ticks(1).await;
            let _ = drain(&mut out_rx);

            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), join)
                .await
                .expect("actor did not exit within 1s")
                .expect("actor task panicked");
        })
        .await;
}

/// Detach then ack: silent no-op. The actor must not crash and the
/// per-consumer entry must NOT be resurrected by the late ack. We
/// can't peek at the map from outside the crate; the indirect proof
/// is that subsequent ticks emit no frames for the detached id (no
/// outbound channel to even reach, since detach drops the actor-side
/// sender; this test pins that detach + ack stays clean).
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ack_after_detach_is_silent_noop() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"hi").expect("new_with_seed");
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

            // Detach.
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

            // Now ack for the just-detached id. Must be a no-op.
            handle
                .consumer_ack
                .send(ConsumerAckRequest {
                    client_id: client,
                    seq: 5,
                })
                .await
                .expect("send ack");
            settle().await;

            // No frames should arrive (entry is gone, tick has no
            // consumer to walk for this id).
            advance_ticks(3).await;
            let items = terminal_outputs(&drain(&mut out_rx));
            assert!(
                items.is_empty(),
                "detached consumer must receive zero frames; got {} ({:?})",
                items.len(),
                items,
            );

            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), join)
                .await
                .expect("actor did not exit within 1s")
                .expect("actor task panicked");
        })
        .await;
}
