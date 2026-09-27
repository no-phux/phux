//! `TerminalActor` driven directly over its channels: the tick scheduler and
//! `FRAME_ACK` routing (ADR-0018), PTY output fanout and replay totality, and
//! child teardown. Field-level ack and tick-emit semantics are unit-tested in
//! `resource/terminal/tests_state_sync.rs`; these pin the channel plumbing.

use std::time::Duration;

use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::{BootstrapId, ClientId, StreamId};
use phux_server::grid::SnapshotBytes;
use phux_server::state::{Outbound, TerminalInput};
use phux_server::terminal_actor::{
    ConsumerAckRequest, ConsumerAttachRequest, ConsumerDetachRequest, DEFAULT_TICK_INTERVAL,
    PaneOutput, ResizeRequest, SnapshotRequest, TerminalActor,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::timeout;

use super::common::{collect_until, fresh, named_key, render_grid, sh};

/// Hang guard only; nothing here measures latency.
const DEADLINE: Duration = Duration::from_secs(30);
const WIRE_TID: u32 = 7;

fn run_local_paused<F: Future<Output = ()>>(fut: F) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, fut);
}

async fn attach(handle: &phux_server::ResourceHandle, id: u32) -> mpsc::Receiver<Outbound> {
    let (outbound, rx) = mpsc::channel(32);
    let (reply, reply_rx) = oneshot::channel();
    handle
        .consumer_attach
        .send(ConsumerAttachRequest {
            client_id: ClientId(id),
            outbound,
            wire_terminal_id: WIRE_TID,
            stream_id: StreamId::new(u64::from(id)).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            wants_state_sync: false,
            state_sync_scrollback: None,
            bootstrap_max_bytes: usize::MAX,
            bootstrap_max_frames: usize::MAX,
            bootstrap_chunk_bytes: 1,
            loss_tolerant: false,
            live_gate: watch::channel(true).1,
            reply,
        })
        .await
        .unwrap();
    reply_rx.await.unwrap().expect("attach succeeded");
    rx
}

async fn ack(handle: &phux_server::ResourceHandle, id: u32, seq: u64) {
    let request = ConsumerAckRequest {
        client_id: ClientId(id),
        stream_id: StreamId::new(u64::from(id)).unwrap(),
        bootstrap_id: BootstrapId::new(1).unwrap(),
        seq,
    };
    handle.consumer_ack.send(request).await.unwrap();
}

async fn advance_ticks(n: u32) {
    for _ in 0..n {
        tokio::time::advance(DEFAULT_TICK_INTERVAL).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }
}

/// `(wire id, seq)` of every `ResourceOutput` queued on `rx`.
fn outputs(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    while let Ok(item) = rx.try_recv() {
        if let Outbound::Frame(FrameKind::ResourceOutput {
            terminal_id, seq, ..
        }) = item
        {
            out.push((terminal_id.local_id().unwrap(), seq));
        }
    }
    out
}

/// Ticks with zero, one, and two consumers stay healthy and stamp each
/// consumer's own seq space from 1; stray, stale, and post-detach acks are
/// silent no-ops that neither crash the actor nor resurrect a detached
/// consumer.
#[test]
fn tick_and_ack_routing_survive_every_consumer_lifecycle_edge() {
    run_local_paused(async {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").unwrap();
        let handle = bundle.handle.clone();
        let token = bundle.token.clone();
        let join = tokio::task::spawn_local(bundle.actor.run());

        advance_ticks(10).await; // no consumers

        let mut a = attach(&handle, 1).await;
        let mut b = attach(&handle, 2).await;
        advance_ticks(1).await;
        for rx in [&mut a, &mut b] {
            for (tid, seq) in outputs(rx) {
                assert_eq!((tid, seq), (WIRE_TID, 1), "own wire id, own seq from 1");
            }
        }

        for (id, seq) in [(1, 5), (1, 3), (1, 5), (1, 4), (999, 42)] {
            ack(&handle, id, seq).await;
        }
        let (reply, reply_rx) = oneshot::channel();
        let detach = ConsumerDetachRequest {
            client_id: ClientId(2),
            reply,
        };
        handle.consumer_detach.send(detach).await.unwrap();
        reply_rx.await.unwrap();
        ack(&handle, 2, 5).await;
        advance_ticks(3).await;

        assert!(
            outputs(&mut b).is_empty(),
            "a detached consumer receives nothing"
        );
        let _ = outputs(&mut a);

        token.cancel();
        timeout(DEADLINE, join).await.expect("actor exits").unwrap();
    });
}

async fn snapshot(handle: &phux_server::ResourceHandle) -> SnapshotBytes {
    let (reply, rx) = oneshot::channel();
    let request = SnapshotRequest {
        scrollback: None,
        max_bytes: usize::MAX,
        max_frames: usize::MAX,
        chunk_bytes: 1,
        reply,
    };
    handle
        .terminal()
        .unwrap()
        .snapshot
        .send(request)
        .await
        .unwrap();
    timeout(DEADLINE, rx).await.unwrap().unwrap().unwrap().0
}

/// Every byte already queued on the broadcast; a lag voids the comparison.
fn drain_pending(rx: &mut broadcast::Receiver<PaneOutput>) -> Vec<u8> {
    let mut acc = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(PaneOutput::Live { bytes, .. } | PaneOutput::Resync { bytes, .. }) => {
                acc.extend_from_slice(&bytes);
            }
            Ok(PaneOutput::Control { .. }) => {}
            Err(broadcast::error::TryRecvError::Lagged(n)) => panic!("broadcast lagged by {n}"),
            Err(_) => return acc,
        }
    }
}

fn replay(cols: u16, rows: u16, bytes: &[u8]) -> Vec<String> {
    let mut t = fresh(cols, rows);
    t.vt_write(bytes);
    render_grid(&t)
}

/// Replay totality (phux-r4k): a snapshot `S0` plus every byte broadcast
/// afterwards reconstructs the server's grid `S1`, with and without a live
/// resize in between (the resize is carried by the re-broadcast resync).
/// The pane prints one atomic block per released `read`, and `S0` is taken
/// before the first release, so there is neither overlap nor gap. A second
/// subscriber proves the broadcast fans the same bytes out to both.
fn assert_replay_totality(resize: Option<(u16, u16)>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let script = "read _; printf '\\r\\nalpha\\r\\nbravo'; \
                      read _; printf '\\r\\ncharlie\\r\\nSENTINEL'; sleep 3600";
        let bundle = TerminalActor::new_with_command(sh(script), 80, 24).unwrap();
        let handle = bundle.handle.clone();
        let token = bundle.token;
        let mut rx = handle.output.subscribe();
        let mut second = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());
        let terminal = handle.terminal().unwrap().clone();
        let release = async || {
            let enter = TerminalInput::Key(named_key(PhysicalKey::Enter));
            terminal.input.send(enter).await.unwrap();
        };

        let s0 = snapshot(&handle).await;
        release().await;
        let mut tail = collect_until(&mut rx, b"bravo").await;
        if let Some((cols, rows)) = resize {
            let request = ResizeRequest {
                cols,
                rows,
                cell_px: None,
                resync_clients: true,
                resync_only: false,
                resync_for: None,
            };
            terminal.resize.send(request).await.unwrap();
        }
        release().await;
        tail.extend(collect_until(&mut rx, b"SENTINEL").await);
        // The resize resync is debounced (50ms) and may trail the sentinel.
        for _ in 0..15 {
            tail.extend(drain_pending(&mut rx));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tail.extend(drain_pending(&mut rx));
        let s1 = snapshot(&handle).await;
        assert_eq!((s1.cols, s1.rows), resize.unwrap_or((80, 24)));

        let mut reconstructed = s0.bytes.clone();
        reconstructed.extend_from_slice(&tail);
        assert_eq!(
            replay(s1.cols, s1.rows, &reconstructed),
            replay(s1.cols, s1.rows, &s1.bytes),
            "S0 + broadcast tail must reconstruct the server grid",
        );
        let fanned = collect_until(&mut second, b"SENTINEL").await;
        assert!(String::from_utf8_lossy(&fanned).contains("alpha"));

        token.cancel();
        timeout(DEADLINE, join).await.expect("actor exits").unwrap();
    });
}

#[test]
fn snapshot_plus_output_reconstructs_server_grid() {
    assert_replay_totality(None);
}

#[test]
fn snapshot_plus_output_reconstructs_server_grid_across_resize() {
    assert_replay_totality(Some((100, 30)));
}

/// Cancel kills and reaps a live child; the actor only exits after
/// `Child::wait` returns.
#[test]
fn shutdown_signal_terminates_long_running_child() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let bundle = TerminalActor::new_with_command(sh("sleep 60"), 80, 24).unwrap();
        let token = bundle.token;
        let join = tokio::task::spawn_local(bundle.actor.run());
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
        timeout(DEADLINE, join)
            .await
            .expect("actor exits (zombie?)")
            .unwrap();
    });
}
