//! phux-3uv: the client `FRAME_ACK` wire loop, end to end over the wire.
//!
//! phux-3uv added the client-side `FRAME_ACK` emission (the v0.1 client
//! sent none). This test drives that loop over a real UDS server with a
//! real PTY-backed pane and asserts the server-side handler accepts it:
//!
//!   1. A fixture prints a unique marker AFTER the client has attached.
//!   2. The client drains `TERMINAL_OUTPUT`, observing the marker exactly
//!      once with a per-consumer monotonic `seq` (SPEC §12.2).
//!   3. The client sends `FRAME_ACK { terminal_id, seq }` for the
//!      delivered frame — the new phux-3uv client behavior, exercised
//!      here on the wire and routed through the server's
//!      `handle_frame_ack -> on_frame_ack -> mark_synced`.
//!   4. The marker is not re-delivered: each PTY byte is emitted once and
//!      acking does not perturb the stream.
//!
//! NOTE on the emitter: `consumer_tick_emits` stays OFF in production
//! (the per-consumer state-sync tick can be the single emitter only once
//! per-consumer dirty isolation is solved — see the field doc on
//! `consumer_tick_emits`, prerequisite 3). So the live emitter here is
//! still the runtime broadcast pump; the single-consumer tick-emit + ack
//! convergence is proven directly at the actor level by the
//! `tick_emit_emits_once_when_gate_is_on` unit test (which can enable the
//! gate). This wire test pins the client ack path + monotonic seq, the
//! parts phux-3uv added that are observable regardless of the gate.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::time::Duration;

use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_TERMINAL_OUTPUT, TYPE_TERMINAL_SNAPSHOT,
};
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
#[allow(
    clippy::too_many_lines,
    reason = "linear attach -> drain -> ack -> reconverge body; splitting would scatter the loop proof"
)]
fn acked_incremental_converges_and_seq_is_monotonic() {
    // Unique marker emitted AFTER attach so it is a live delta.
    const MARKER: &[u8] = b"PHUX3UVACKMARKER";
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Give the test time to attach before the marker is printed, so the
        // marker lands strictly as a post-attach live delta rather than in
        // the snapshot. Attach completes in well under 100 ms; 0.5 s keeps
        // margin for a loaded 2-core CI runner.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 0.5; printf PHUX3UVACKMARKER; sleep 30");

        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        assert!(
            matches!(attached, FrameKind::Attached { .. }),
            "expected Attached",
        );

        // Drain the snapshot. It must NOT carry the marker (printed only
        // after the 0.5s sleep, i.e. after attach + prime).
        let (snap_tb, snap) = recv_typed(&mut stream).await;
        if snap_tb == TYPE_TERMINAL_SNAPSHOT
            && let FrameKind::TerminalSnapshot {
                vt_replay_bytes, ..
            } = snap
        {
            assert_eq!(
                count_occurrences(&vt_replay_bytes, MARKER),
                0,
                "marker must arrive as a live delta, not in the snapshot",
            );
        }

        // Phase 1: wait for the marker to land as a live TERMINAL_OUTPUT
        // delta. Capture its terminal_id + seq for the ack, and assert the
        // per-consumer seq is strictly increasing across frames.
        let mut last_seq: Option<u64> = None;
        let mut ack_target: Option<(phux_protocol::ids::TerminalId, u64)> = None;
        let phase1_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while tokio::time::Instant::now() < phase1_deadline {
            let remaining = phase1_deadline - tokio::time::Instant::now();
            let Ok((tb, frame)) = timeout(remaining, recv_typed(&mut stream)).await else {
                break;
            };
            if tb != TYPE_TERMINAL_OUTPUT {
                continue;
            }
            let FrameKind::TerminalOutput {
                terminal_id,
                seq,
                bytes,
            } = frame
            else {
                continue;
            };
            if let Some(prev) = last_seq {
                assert!(
                    seq > prev,
                    "per-consumer seq must be strictly monotonic (SPEC 12.2): \
                     got seq={seq} after prev={prev}",
                );
            }
            last_seq = Some(seq);
            if count_occurrences(&bytes, MARKER) >= 1 {
                ack_target = Some((terminal_id, seq));
                break;
            }
        }

        let (ack_terminal, ack_seq) =
            ack_target.expect("marker must be delivered as a live TERMINAL_OUTPUT delta");

        // Phase 2: ack the delivered frame (the new phux-3uv client
        // behavior). The server routes it through handle_frame_ack ->
        // on_frame_ack -> mark_synced; the handler must accept it without
        // perturbing the stream.
        send_frame(
            &mut stream,
            &FrameKind::FrameAck {
                terminal_id: ack_terminal,
                seq: ack_seq,
            },
        )
        .await;

        // Phase 3: drain for well past many emission intervals (~25 ticks
        // at the 30ms cadence). The marker must NOT be re-delivered: each
        // PTY byte is emitted once and the ack does not perturb the stream.
        // (Under the broadcast pump the marker is a single chunk; the
        // assertion would also catch a double-emitting build.)
        let mut post_ack: Vec<u8> = Vec::new();
        let drain_deadline = tokio::time::Instant::now() + Duration::from_millis(800);
        while tokio::time::Instant::now() < drain_deadline {
            let remaining = drain_deadline - tokio::time::Instant::now();
            match timeout(remaining, recv_typed(&mut stream)).await {
                Ok((tb, frame)) => {
                    if tb == TYPE_TERMINAL_OUTPUT
                        && let FrameKind::TerminalOutput { seq, bytes, .. } = frame
                    {
                        if let Some(prev) = last_seq {
                            assert!(
                                seq > prev,
                                "post-ack seq must stay strictly monotonic: \
                                 got seq={seq} after prev={prev}",
                            );
                        }
                        last_seq = Some(seq);
                        post_ack.extend_from_slice(&bytes);
                    }
                }
                Err(_) => break,
            }
        }

        assert_eq!(
            count_occurrences(&post_ack, MARKER),
            0,
            "after FRAME_ACK the dirty cache is cleared; the marker must not \
             be re-emitted. Re-delivery means the acked-incremental loop did \
             not converge. Post-ack bytes: {:?}",
            String::from_utf8_lossy(&post_ack),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        let _ = timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle).await;
    });
}
