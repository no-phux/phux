//! Scheduling work counts, not machine-load-sensitive timing ceilings.

use futures_util::FutureExt;

use super::*;
use crate::resource::ControlRequest;
use crate::resource::terminal::test_support::dummy_outbound;
use crate::resource::terminal::{
    ConsumerAckRequest, ConsumerDetachRequest, ProcessFacetRequest, PwdRequest, SnapshotRequest,
};
use phux_protocol::ClientId;
use tokio::sync::oneshot;

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn idle_subscribed_actors_do_not_consume_tick_deadlines() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for count in [1, 32, 128] {
                let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel(count);
                let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
                let actors: Vec<_> = (0..count)
                    .map(|_| {
                        tokio::task::spawn_local(idle_subscribed_actor(
                            ready_tx.clone(),
                            stop_rx.clone(),
                        ))
                    })
                    .collect();
                drop(ready_tx);
                for _ in 0..count {
                    ready_rx
                        .recv()
                        .await
                        .expect("every actor has entered its loop");
                }
                assert!(
                    actors.iter().all(|actor| !actor.is_finished()),
                    "all actors remain active together"
                );
                tokio::time::advance(std::time::Duration::from_secs(1)).await;
                stop_tx.send(true).expect("stop observation");
                for actor in actors {
                    actor.await.expect("idle actor assertion");
                }
            }
        })
        .await;
}

#[allow(
    clippy::future_not_send,
    reason = "test actors own !Send terminals on a LocalSet"
)]
async fn idle_subscribed_actor(
    ready: tokio::sync::mpsc::Sender<()>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let (outbound, _output) = dummy_outbound();
    actor
        .register_consumer(ClientId(1), outbound, 11, true)
        .expect("consumer");
    actor.tick_emit();
    let mut state = actor.arm_run_loop().await;
    let started = tokio::time::Instant::now();
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    {
        let run = actor.drive_run_loop(&mut state, deadline.as_mut());
        tokio::pin!(run);
        assert!(futures_util::poll!(run.as_mut()).is_pending());
        ready
            .try_send(())
            .expect("ready channel holds one receipt per actor");
        tokio::select! {
            biased;
            () = run.as_mut() => panic!("idle actor unexpectedly stopped"),
            result = stop.changed() => result.expect("observation ended"),
        }
        // Even if a stop receipt wins a prior task wake, explicitly poll the
        // still-live loop after the shared advance before examining its timer.
        assert!(futures_util::poll!(run.as_mut()).is_pending());
    }
    assert!(tokio::time::Instant::now() >= started + std::time::Duration::from_secs(1));
    let unconsumed = state
        .tick
        .tick()
        .now_or_never()
        .expect("the original deadline is still owed");
    assert_eq!(
        unconsumed,
        started + state.tick_interval,
        "idle loop must not consume its state tick while all actors are active"
    );
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn native_cursor_expiry_runs_through_the_armed_loop_timer() {
    use crate::resource::terminal::{NATIVE_HISTORY_TTL, NativeCursorKey, NativeCursorOwner};
    use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
    use phux_protocol::wire::frame::HistoryTombstoneReason;

    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let stream_id = StreamId::new(1).expect("stream");
    actor.native_cursor_owners.insert(
        NativeCursorKey::new(7, stream_id),
        NativeCursorOwner {
            cursor: [0; 32],
            record_index: 0,
            touched: tokio::time::Instant::now(),
            next_page_seq: 1,
            terminal_id: ResourceId::local(1),
            stream_id,
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
        },
    );
    assert!(actor.state_tick_armed(), "a cursor alone arms the timer");
    let mut controls = actor.core.output_tx.subscribe();
    let mut state = actor.arm_run_loop().await;
    let interval = state.tick_interval;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    {
        let run = actor.drive_run_loop(&mut state, deadline.as_mut());
        tokio::pin!(run);
        assert!(futures_util::poll!(run.as_mut()).is_pending());
        tokio::time::advance(
            NATIVE_HISTORY_TTL
                .checked_sub(interval)
                .expect("TTL exceeds tick"),
        )
        .await;
        assert!(futures_util::poll!(run.as_mut()).is_pending());
        assert!(controls.try_recv().is_err(), "no premature cursor expiry");
        tokio::time::advance(interval).await;
        assert!(futures_util::poll!(run.as_mut()).is_pending());
        assert!(matches!(
            controls.try_recv().expect("timer dispatched expiry"),
            PaneOutput::Control {
                frame: FrameKind::HistoryTombstone {
                    reason: HistoryTombstoneReason::Expired,
                    ..
                },
                ..
            }
        ));
    }
    assert!(
        actor.native_cursor_owners.is_empty(),
        "expiry releases its timer obligation"
    );
    assert!(
        !actor.state_tick_armed(),
        "last cursor expiry disarms the quiet actor"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn service_rotation_cannot_be_monopolized_by_snapshot_requests() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let facet = bundle.handle.terminal().expect("terminal");
    let mut snapshots = Vec::new();
    for _ in 0..8 {
        let (reply, replied) = oneshot::channel();
        facet
            .snapshot
            .try_send(SnapshotRequest {
                scrollback: None,
                max_bytes: usize::MAX,
                max_frames: usize::MAX,
                chunk_bytes: 1,
                reply,
            })
            .expect("snapshot");
        snapshots.push(replied);
    }
    let (reply, mut cwd) = oneshot::channel();
    facet.pwd.try_send(PwdRequest { reply }).expect("cwd");
    let (reply, mut process) = oneshot::channel();
    facet
        .process
        .try_send(ProcessFacetRequest { reply })
        .expect("process");
    let (reply, mut detached) = oneshot::channel();
    bundle
        .handle
        .consumer_detach
        .try_send(ConsumerDetachRequest {
            client_id: ClientId(99),
            reply,
        })
        .expect("detach");
    let (reply, mut control) = oneshot::channel();
    bundle
        .handle
        .control
        .try_send(ControlRequest::ReportStreamState { state: None, reply })
        .expect("control");
    let (outbound, mut output) = dummy_outbound();
    actor
        .register_consumer(ClientId(1), outbound, 11, true)
        .expect("consumer");
    actor.enable_loss_tolerance_for_test(ClientId(1));
    actor.vt_write_for_test(b"pending");
    actor.tick_emit();
    let stream = actor.consumer_state(ClientId(1)).expect("state");
    bundle
        .handle
        .consumer_ack
        .try_send(ConsumerAckRequest {
            client_id: ClientId(1),
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            seq: 1,
        })
        .expect("ack");
    while output.try_recv().is_ok() {}
    let mut state = actor.arm_run_loop().await;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    for _ in 0..6 {
        actor.service_pending_turn(&mut state, deadline.as_mut());
    }
    assert!(snapshots[0].try_recv().is_ok());
    assert!(
        snapshots[1].try_recv().is_err(),
        "rotation visits other ready classes before snapshot again"
    );
    assert!(cwd.try_recv().is_ok());
    assert!(process.try_recv().is_ok());
    assert!(detached.try_recv().is_ok());
    assert!(control.try_recv().is_ok());
    assert_eq!(
        actor
            .consumer_state(ClientId(1))
            .expect("state")
            .last_acked_seq,
        1
    );
    assert!(
        !actor.state_tick_armed(),
        "ACK clears the last retransmit obligation"
    );
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn service_rotation_keeps_snapshot_gated_during_native_capture() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let mut state = actor.arm_run_loop().await;
    let (reply, _capture) = oneshot::channel();
    actor.start_native_bootstrap(crate::resource::terminal::NativeBootstrapRequest {
        owner: 31,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: phux_protocol::ids::StreamId::new(1).expect("stream"),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap"),
        limits: phux_protocol::caps::BootstrapLimits::default(),
        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
        max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
        reply,
    });
    assert!(
        actor.native_bootstrap_pending(),
        "real capture owns the grid"
    );
    let (reply, mut snapshot) = oneshot::channel();
    bundle
        .handle
        .terminal()
        .expect("terminal")
        .snapshot
        .try_send(SnapshotRequest {
            scrollback: None,
            max_bytes: usize::MAX,
            max_frames: usize::MAX,
            chunk_bytes: 1,
            reply,
        })
        .expect("snapshot");
    let (reply, mut cwd) = oneshot::channel();
    bundle
        .handle
        .terminal()
        .expect("terminal")
        .pwd
        .try_send(PwdRequest { reply })
        .expect("cwd");
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    actor.service_pending_turn(&mut state, deadline.as_mut());
    assert!(
        cwd.try_recv().is_ok(),
        "metadata remains responsive during capture"
    );
    assert!(
        snapshot.try_recv().is_err(),
        "snapshot cannot touch the frozen grid"
    );
    actor.land_native_cuts();
    actor.service_pending_turn(&mut state, deadline.as_mut());
    assert!(snapshot.try_recv().expect("reply after capture").is_ok());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn service_rotation_services_due_state_tick_under_request_pressure() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let (outbound, mut output) = dummy_outbound();
    actor
        .register_consumer(ClientId(1), outbound, 11, true)
        .expect("consumer");
    actor.vt_write_for_test(b"tick owed");
    let mut state = actor.arm_run_loop().await;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    tokio::time::advance(state.tick_interval).await;
    for _ in 0..2 {
        let (reply, _replied) = oneshot::channel();
        bundle
            .handle
            .terminal()
            .expect("terminal")
            .snapshot
            .try_send(SnapshotRequest {
                scrollback: None,
                max_bytes: usize::MAX,
                max_frames: usize::MAX,
                chunk_bytes: 1,
                reply,
            })
            .expect("snapshot");
        actor.service_pending_turn(&mut state, deadline.as_mut());
    }
    assert!(
        output.try_recv().is_ok(),
        "due timer gets a turn before a second queued snapshot"
    );
    assert!(!actor.state_tick_armed());
}
