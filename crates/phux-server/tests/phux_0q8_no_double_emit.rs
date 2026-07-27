//! phux-0q8 adversarial regression: prove the live terminal output path
//! is NOT doubled by the newly-wired per-consumer state-sync lifecycle.
//!
//! The change in 0ee4844 registers a per-consumer `RenderState` at ATTACH
//! and frees it at DETACH. The hazard is double-emit: if the actor's
//! `tick_emit` were NOT gated off, an attached consumer would receive the
//! same content TWICE — once from the live broadcast pump and once from
//! the tick-synthesized diff. This test stands up a real UDS server with a
//! real PTY-backed pane that prints a unique marker EXACTLY ONCE, attaches
//! a real wire client, drains `TERMINAL_OUTPUT` for many tick intervals
//! (tick = 30ms), and asserts the marker is seen EXACTLY ONCE.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::time::Duration;

use phux_protocol::wire::frame::{FrameKind, TYPE_ATTACHED, TYPE_TERMINAL_OUTPUT};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// Count non-overlapping occurrences of `needle` in `hay`.
fn count_occurrences(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

#[test]
fn live_output_is_delivered_exactly_once() {
    // Deterministic single-shot fixture marker: unique, and containing no
    // chars a terminal would rewrite.
    const MARKER: &[u8] = b"PHUX0Q8MARKERUNIQUE";
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Print the marker EXACTLY once, then block so the pane stays alive
        // (no EOF-detach, no shell prompt noise, no re-echo).
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("printf PHUX0Q8MARKERUNIQUE; sleep 30");

        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let snapshot_marker_count = match attached {
            FrameKind::Attached { .. } => 0usize,
            other => panic!("expected Attached, got {other:?}"),
        };

        // The TERMINAL_SNAPSHOT may legitimately carry the marker once
        // (the pane already printed it before we attached). Count that
        // separately: the snapshot is the canonical one-time replay and
        // is NOT part of the live-output doubling hazard. The hazard is
        // duplicate TERMINAL_OUTPUT deltas.
        let (snap_tb, snap) = recv_typed(&mut stream).await;
        let mut snapshot_count = snapshot_marker_count;
        if snap_tb == phux_protocol::wire::frame::TYPE_TERMINAL_SNAPSHOT
            && let FrameKind::TerminalSnapshot {
                vt_replay_bytes, ..
            } = snap
        {
            snapshot_count += count_occurrences(&vt_replay_bytes, MARKER);
        }

        // Drain live TERMINAL_OUTPUT for well past many tick intervals
        // (tick = 30ms). A double-emitting tick would re-paint the dirty
        // grid every 30ms, so over a ~1.5s window a regression yields
        // dozens of extra marker copies. A correct (gated) build yields
        // ZERO extra: the broadcast pump emits the PTY bytes once and the
        // gated tick stays silent.
        let mut output_acc: Vec<u8> = Vec::new();
        let drain_deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
        while tokio::time::Instant::now() < drain_deadline {
            let remaining = drain_deadline - tokio::time::Instant::now();
            match timeout(remaining, recv_typed(&mut stream)).await {
                Ok((tb, frame)) => {
                    if tb == TYPE_TERMINAL_OUTPUT
                        && let FrameKind::TerminalOutput { bytes, .. } = frame
                    {
                        output_acc.extend_from_slice(&bytes);
                    }
                }
                Err(_) => break, // window elapsed with no more frames — expected steady state
            }
        }

        let live_count = count_occurrences(&output_acc, MARKER);
        let total = snapshot_count + live_count;

        // The marker must appear EXACTLY ONCE across the whole attached
        // session: either in the one-time snapshot replay OR as a single
        // live delta, never both, never doubled.
        assert_eq!(
            total,
            1,
            "marker must be delivered EXACTLY ONCE (snapshot={snapshot_count}, \
             live_output={live_count}); a count > 1 means the gated tick is \
             double-emitting alongside the broadcast pump (phux-0q8 regression). \
             Live output bytes: {:?}",
            String::from_utf8_lossy(&output_acc),
        );

        // Defense in depth: no live TERMINAL_OUTPUT delta should EVER
        // re-carry the marker once the snapshot has it. If the snapshot
        // captured the marker, live deltas must contain zero copies.
        if snapshot_count == 1 {
            assert_eq!(
                live_count, 0,
                "snapshot already carried the marker; any live re-delivery is \
                 a double-emit (phux-0q8 regression)",
            );
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        let _ = timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle).await;
    });
}
