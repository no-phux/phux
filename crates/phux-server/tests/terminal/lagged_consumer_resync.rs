//! phux-l96p.10: a consumer that falls far enough behind must still converge.
//!
//! The pane's output broadcast is bounded, so a consumer the server cannot
//! drain fast enough eventually takes a `RecvError::Lagged` and its pump misses
//! a window of `TERMINAL_OUTPUT`. What the pump sends *next* is the whole
//! story: the session kernel applies live output strictly in sequence, so a
//! `TERMINAL_OUTPUT` whose `seq` skips the dropped window is a protocol error,
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
//! a deliberately slow consumer, forces the lag, and asserts the two things
//! that separate "recovers" from "never comes back":
//!
//! * every `TERMINAL_OUTPUT` it receives is exactly the next `seq` its
//!   generation expects — the same rule `phux-client-core`'s kernel enforces,
//!   so "no gap here" means "the real client would not have detached"; and
//! * it ends up holding the pane's *current* screen, reached through a
//!   replacement generation rather than a stale one.
//!
//! Before the fix this fails on the first assertion, seconds into the drain.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::doc_markdown, reason = "tests")]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use phux_protocol::ids::{BootstrapId, StreamId, TerminalId};
use phux_protocol::wire::frame::FrameKind;
use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;

/// What the seed pane prints when it is finished. Converging on this, rather
/// than on any intermediate line, is what makes "the consumer holds the
/// current screen" an assertion rather than a hope.
const TAIL_MARKER: &str = "LAGTEST_DONE";

/// The seed pane's workload: paced bursts, so output is still flowing after
/// the stall below on any machine this suite runs on. Each burst is ~0.7 MB,
/// well past the 256-frame broadcast window, and the pacing keeps the whole
/// run around six seconds instead of finishing before the consumer stalls.
const BURST_CMD: &str = "for i in 1 2 3 4 5 6; do seq 1 100000; sleep 0.5; done; echo LAGTEST_DONE";

/// How long the consumer reads nothing at all. Megabytes are produced over
/// this window while the per-client mailbox (8 frames) and the socket buffer
/// are both full, which is what pushes the pump off the broadcast.
const STALL: Duration = Duration::from_secs(2);

/// Ceiling on the post-stall drain. Generous on purpose: the assertion is that
/// the consumer converges at all, never how fast.
const CONVERGE_DEADLINE: Duration = Duration::from_secs(90);

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// Identity of one bootstrap generation on the wire.
type Generation = (TerminalId, StreamId, BootstrapId);

/// Per-generation live-sequence expectation, mirroring the client kernel's
/// `expect_next_seq`: a bootstrap sets the base, and every subsequent
/// `TERMINAL_OUTPUT` on that generation must be the very next sequence.
#[derive(Default)]
struct SequenceOracle {
    next: HashMap<Generation, u64>,
    generations: usize,
}

impl SequenceOracle {
    fn open(&mut self, key: Generation, base_seq: u64) {
        self.next.insert(key, base_seq.saturating_add(1));
        self.generations += 1;
    }

    /// Panics with the diagnosis the real client would have printed.
    fn observe(&mut self, key: &Generation, seq: u64) {
        let expected = self.next.get_mut(key).unwrap_or_else(|| {
            panic!("TERMINAL_OUTPUT seq={seq} names a generation that was never opened")
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
        FrameKind::TerminalOutput {
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
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", BURST_CMD]);
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

        // The stall. Nothing is read from the socket, so the writer task
        // blocks, the mailbox fills, and the pump falls off the broadcast.
        tokio::time::sleep(STALL).await;

        // Resume and drain until the pane's last line is on screen. Every
        // frame is checked on the way through, so a single gapped `seq` fails
        // here exactly as the real client would have.
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < CONVERGE_DEADLINE,
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

        drop(stream);
        let _ = shutdown.send(());
        let _ = server.await;
    });
}
