//! phux-l96p.10: an ATTACH consumer that lags the bounded output broadcast
//! must converge on a replacement bootstrap generation, never see a live
//! `seq` gap (the client kernel detaches on one). The lag is driven, not
//! timed (phux-8kpb): the pane dumps only after the test opens a gate, the
//! test reads nothing until the server's own grid shows the dump's tail, and
//! the ring is shrunk so the stalled pump is certainly lapped.
//! `attach/lagged_attach_terminal_resync.rs` pins the `ATTACH_RESOURCE` pump.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::FrameKind;
use phux_server_testkit::screen::Screen;
use phux_server_testkit::tracing_capture::TracingCapture;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_server_screen_text, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;

/// Last line the pane prints; converging on it proves the screen is current.
const TAIL_MARKER: &str = "LAGTEST_DONE";

/// ~6.9 MB: several times what the stalled consumer can buffer.
const DUMP_LINES: u32 = 1_000_000;

/// Shrunk ring so the dump laps the stalled pump (production: 256).
const TEST_OUTPUT_BROADCAST: usize = 4;

/// Hang guard; every wait ends on a specific frame or screen state.
const HANG_GUARD: Duration = Duration::from_secs(60);

const COLS: u16 = 80;
const ROWS: u16 = 24;

type Generation = (ResourceId, StreamId, BootstrapId);

/// Wait for `gate`, dump, print the marker, then stay alive (an exited pane
/// would be reaped along with any resync owed to a fenced pump).
fn burst_cmd(gate: &Path) -> String {
    format!(
        "while [ ! -e '{}' ]; do sleep 0.02; done; seq 1 {DUMP_LINES}; \
         echo {TAIL_MARKER}; exec sleep 3600",
        gate.display(),
    )
}

/// Per-generation `expect_next_seq`, as the client kernel enforces it.
#[derive(Default)]
struct SequenceOracle {
    next: HashMap<Generation, u64>,
    generations: usize,
    pane: Option<ResourceId>,
}

impl SequenceOracle {
    fn open(&mut self, key: Generation, base_seq: u64) {
        self.pane = Some(key.0.clone());
        self.next.insert(key, base_seq.saturating_add(1));
        self.generations += 1;
    }

    fn observe(&mut self, key: &Generation, seq: u64) {
        let expected = self.next.get_mut(key).unwrap_or_else(|| {
            panic!("RESOURCE_OUTPUT seq={seq} names a generation that was never opened")
        });
        assert_eq!(
            seq, *expected,
            "live sequence gap at {seq}; expected {expected} — the session kernel \
             rejects this frame and the client detaches with a protocol error",
        );
        *expected = seq.saturating_add(1);
    }
}

enum Applied {
    BootstrapReady,
    Fatal(String),
    Other,
}

fn apply(frame: &FrameKind, oracle: &mut SequenceOracle, screen: &mut Screen) -> Applied {
    match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            base_seq,
            ..
        } => {
            oracle.open((terminal_id.clone(), *stream_id, *bootstrap_id), *base_seq);
            Applied::Other
        }
        FrameKind::BootstrapChunk { payload, .. } => {
            screen.write(payload);
            Applied::Other
        }
        FrameKind::BootstrapReady { .. } => Applied::BootstrapReady,
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => {
            oracle.observe(&(terminal_id.clone(), *stream_id, *bootstrap_id), *seq);
            screen.write(bytes);
            Applied::Other
        }
        FrameKind::Detached { reason, message } => {
            Applied::Fatal(format!("DETACHED reason={reason:?} message={message}"))
        }
        FrameKind::Error { code, message, .. } => {
            Applied::Fatal(format!("ERROR code={code:?} message={message}"))
        }
        _ => Applied::Other,
    }
}

#[test]
fn lagged_consumer_converges_on_a_replacement_generation() {
    phux_server::resource::set_output_broadcast_capacity_for_test(TEST_OUTPUT_BROADCAST);
    run_local(async {
        // Include actor diagnostics when convergence fails.
        let _cap = TracingCapture::install("lagged_consumer_resync");
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let gate = tmp.path().join("dump.gate");
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", &burst_cmd(&gate)]);
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "lag", cmd);

        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("lag")).await;

        let mut oracle = SequenceOracle::default();
        let mut screen = Screen::new(COLS, ROWS).expect("screen oracle");

        loop {
            let (_, frame) = recv_typed(&mut stream).await;
            if matches!(
                apply(&frame, &mut oracle, &mut screen),
                Applied::BootstrapReady
            ) {
                break;
            }
        }
        let pane = oracle
            .pane
            .clone()
            .expect("the opening bootstrap names its pane");

        // Stall: read nothing until the server's grid shows the dump's tail.
        // The probe has no subscription, so polling it unblocks nothing.
        let mut probe = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        std::fs::write(&gate, b"").expect("open the dump gate");
        wait_for_server_screen_text(&mut probe, &pane, TAIL_MARKER, HANG_GUARD).await;

        let started = Instant::now();
        let mut received = 0usize;
        loop {
            assert!(
                started.elapsed() < HANG_GUARD,
                "consumer never converged after the broadcast gap \
                 ({received} frames in {:?})",
                started.elapsed(),
            );
            let (_, frame) = recv_typed(&mut stream).await;
            received += 1;
            if let Applied::Fatal(what) = apply(&frame, &mut oracle, &mut screen) {
                panic!(
                    "server ended the session instead of resyncing after \
                     {received} frames in {:?}: {what}",
                    started.elapsed()
                );
            }
            if screen.contains(TAIL_MARKER) {
                break;
            }
        }

        // One generation would mean the run never lagged.
        assert!(
            oracle.generations > 1,
            "consumer converged without ever taking a broadcast gap",
        );

        drop(probe);
        drop(stream);
        let _ = shutdown.send(());
        let _ = server.await;
    });
}
