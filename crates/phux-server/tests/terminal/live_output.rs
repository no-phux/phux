//! Live output is delivered exactly once with a strictly monotonic seq, and
//! a client `FRAME_ACK` (phux-3uv) neither re-delivers nor perturbs it. A
//! gated state-sync tick double-emitting alongside the broadcast pump
//! (phux-0q8) would repaint the marker every tick.

use std::time::Duration;

use phux_protocol::wire::frame::FrameKind;
use tempfile::TempDir;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

use super::common::{count, sh};

#[test]
fn acked_live_output_is_delivered_once_with_monotonic_seq() {
    const MARKER: &[u8] = b"PHUX3UVACKMARKER";
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let go = tmp.path().join("go");
        // The seed prints only after the test has drained the bootstrap, so
        // the marker is provably a live delta (a timed sleep flaked, phux-285q).
        let mut cmd = sh(
            "until [ -e \"$PHUX_TEST_GO\" ]; do sleep 0.05; done; printf PHUX3UVACKMARKER; sleep 30",
        );
        cmd.env("PHUX_TEST_GO", &go);
        let (shutdown, _server) = spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;
        loop {
            match recv_typed(&mut stream).await.1 {
                FrameKind::BootstrapChunk { payload, .. } => assert_eq!(count(&payload, MARKER), 0),
                FrameKind::BootstrapReady { .. } => break,
                _ => {}
            }
        }
        std::fs::write(&go, b"go").unwrap();

        let mut last_seq = None;
        let mut delivered = Vec::new();
        let mut acked = false;
        let mut quiet_until = None;
        loop {
            let deadline =
                quiet_until.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(8));
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Ok((_, frame)) = timeout(remaining, recv_typed(&mut stream)).await else {
                break;
            };
            let FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } = frame
            else {
                continue;
            };
            if let Some(prev) = last_seq {
                assert!(
                    seq > prev,
                    "seq must be strictly monotonic: {seq} after {prev}"
                );
            }
            last_seq = Some(seq);
            delivered.extend_from_slice(&bytes);
            if !acked && count(&delivered, MARKER) >= 1 {
                let ack = FrameKind::FrameAck {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                };
                send_frame(&mut stream, &ack).await;
                acked = true;
                // ~25 tick intervals: long enough for any re-emission.
                quiet_until = Some(tokio::time::Instant::now() + Duration::from_millis(800));
            }
        }
        assert!(acked, "marker never arrived as a live delta");
        assert_eq!(
            count(&delivered, MARKER),
            1,
            "marker delivered more than once: {:?}",
            String::from_utf8_lossy(&delivered),
        );
        drop(shutdown);
    });
}
