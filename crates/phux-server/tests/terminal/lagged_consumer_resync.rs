//! phux-l96p.10: a consumer that falls far enough behind must still converge.
//!
//! The pane's output broadcast is bounded, so a consumer the server cannot
//! drain fast enough eventually takes a `RecvError::Lagged` and its pump misses
//! a window of `RESOURCE_OUTPUT`. What the pump sends *next* is the whole
//! story: the session kernel applies live output strictly in sequence, so a
//! `RESOURCE_OUTPUT` whose `seq` skips the dropped window is a protocol error,
//! not a hiccup. The real client detaches on it — "live sequence gap at N;
//! expected M" — and the pane goes dark for good. That is what a remote
//! WebSocket attach hit on `seq 1 300000`.
//!
//! The server's answer is an in-band resync: the pump asks the actor to
//! re-broadcast the whole grid and republishes it as a fresh bootstrap
//! generation. The answer only works if the pump stops forwarding the *old*
//! generation's live frames the moment the gap opens — otherwise the client is
//! already gone, and a pump still awaiting mailbox capacity for frames nobody
//! can use consumes the broadcast at the client's speed, which is exactly how
//! the resync it just asked for gets overwritten before it arrives.
//!
//! This test drives the production `handle_client` loop over the real wire with
//! a deliberately stalled consumer, forces the lag, and asserts the two things
//! that separate "recovers" from "never comes back":
//!
//! * every `RESOURCE_OUTPUT` it receives is exactly the next `seq` its
//!   generation expects — the same rule `phux-client-core`'s kernel enforces,
//!   so "no gap here" means "the real client would not have detached"; and
//! * it ends up holding the pane's *current* screen, reached through a
//!   replacement generation rather than a stale one.
//!
//! The lag is driven, not timed (phux-8kpb): the pane dumps only once the test
//! opens a gate, the test reads nothing until the server's own grid shows the
//! dump's last line, and the ring is shrunk to [`TEST_OUTPUT_BROADCAST`] slots,
//! so the stalled pump has certainly been lapped. The pane then stays alive
//! and quiet, so the resync is always answered and never overwritten. See
//! `lagged_attach_terminal_resync.rs` for the race the timed shape had.
//!
//! Before the fix this fails on the first assertion, seconds into the drain.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::doc_markdown, reason = "tests")]

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::FrameKind;
use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_server_screen_text, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;

/// What the seed pane prints when it is finished. Converging on this, rather
/// than on any intermediate line, is what makes "the consumer holds the
/// current screen" an assertion rather than a hope.
const TAIL_MARKER: &str = "LAGTEST_DONE";

/// Lines the pane dumps once the gate opens: ~6.9 MB, several times what the
/// stalled consumer's mailbox, writer batch and socket buffers can hold.
const DUMP_LINES: u32 = 1_000_000;

/// Broadcast ring used by this test, so the dump laps the stalled pump however
/// the reader coalesces it. Production stays at 256.
const TEST_OUTPUT_BROADCAST: usize = 4;

/// Bound on each event-driven wait below. A hang guard, never a timing
/// assertion: every wait ends on a specific frame or screen state.
const HANG_GUARD: Duration = Duration::from_secs(60);

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// Identity of one bootstrap generation on the wire.
type Generation = (ResourceId, StreamId, BootstrapId);

/// The seed pane's workload: wait for `gate`, dump, print the marker, then
/// stay alive. A pane that exited would be reaped — taking the session, and
/// any resync still owed to a fenced pump, with it.
fn burst_cmd(gate: &Path) -> String {
    format!(
        "while [ ! -e '{}' ]; do sleep 0.02; done; seq 1 {DUMP_LINES}; \
         echo {TAIL_MARKER}; exec sleep 3600",
        gate.display(),
    )
}

/// Per-generation live-sequence expectation, mirroring the client kernel's
/// `expect_next_seq`: a bootstrap sets the base, and every subsequent
/// `RESOURCE_OUTPUT` on that generation must be the very next sequence.
#[derive(Default)]
struct SequenceOracle {
    next: HashMap<Generation, u64>,
    generations: usize,
    /// The pane the most recent bootstrap opened, so the test can ask the
    /// server for that pane's own screen.
    pane: Option<ResourceId>,
}

impl SequenceOracle {
    fn open(&mut self, key: Generation, base_seq: u64) {
        self.pane = Some(key.0.clone());
        self.next.insert(key, base_seq.saturating_add(1));
        self.generations += 1;
    }

    /// Panics with the diagnosis the real client would have printed.
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

/// What one drained frame meant to the consumer.
enum Applied {
    /// A generation finished publishing.
    BootstrapReady,
    /// The server ended the session instead of resyncing.
    Fatal(String),
    /// Anything else, already folded into the oracle and the screen.
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

        // Drain the opening bootstrap so the pump is live before the stall.
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

        // The stall, driven rather than timed. Nothing is read from the
        // attached socket until the pane's server-side grid shows the end of
        // the dump, so the writer blocks, the mailbox fills, and the rest of
        // the dump laps the ring. The probe carries no subscription, so
        // polling it unblocks nothing.
        let mut probe = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        std::fs::write(&gate, b"").expect("open the dump gate");
        wait_for_server_screen_text(&mut probe, &pane, TAIL_MARKER, HANG_GUARD).await;

        // Resume and drain until the pane's last line is on screen. Every
        // frame is checked on the way through, so a single gapped `seq` fails
        // here exactly as the real client would have.
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < HANG_GUARD,
                "consumer never converged after the broadcast gap",
            );
            let (_, frame) = recv_typed(&mut stream).await;
            if let Applied::Fatal(what) = apply(&frame, &mut oracle, &mut screen) {
                panic!("server ended the session instead of resyncing: {what}");
            }
            if screen.contains(TAIL_MARKER) {
                break;
            }
        }

        // A gap answered by a resync opens a *new* generation; the opening
        // bootstrap alone would leave this at one, which would mean the run
        // never actually lagged and the test proved nothing.
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
