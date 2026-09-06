//! phux-l96p.10, second pump: an `ATTACH_TERMINAL` consumer that falls behind
//! must converge, exactly like a session-attached one.
//!
//! `ATTACH_TERMINAL` has its own output pump (`runtime::commands`), separate
//! from the ATTACH pump in `runtime::attach`. When the broadcast gap fence was
//! added it went into one of them, and this is the path that did not get it:
//! `phux rec`, `phux play`, headless pane watchers, the FFI and mobile
//! consumers, and a federation hub's proxy subscription all arrive here. Its
//! lag handler asked the actor for a resync and then resumed forwarding live
//! deltas immediately, putting a `TERMINAL_OUTPUT` whose `seq` skips the
//! dropped window on the wire — which the consumer's session kernel rejects as
//! a protocol error, killing the consumer before the resync can land.
//!
//! The assertions mirror `lagged_consumer_resync.rs` frame for frame, because
//! the guarantee is the same guarantee; only the subscription verb differs.
//! Before the fix this fails on the first one, seconds into the drain.
//!
//! One trap this test fell into once, worth naming: the bootstrap arrives
//! *interleaved ahead of* the `COMMAND_RESULT` that answers `ATTACH_TERMINAL`,
//! so a helper that loops discarding everything but the result swallows the
//! whole opening generation, and the stream then looks like it began with a
//! bare `TERMINAL_OUTPUT`. It does not: ADR-0007 s4 requires the snapshot to
//! precede every delta and the server honours it (`hub_relay_federation.rs`
//! asserts the same ordering through a hub). [`attach_terminal_only`] returns
//! the interleaved frames for exactly that reason, and the oracle below is
//! strict — output for a generation no bootstrap opened is a failure, not a
//! case to accommodate.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::doc_markdown, reason = "tests")]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use phux_protocol::ids::{BootstrapId, GroupId, StreamId, TerminalId};
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, SpawnResult};
use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

/// What the bursting pane prints when it is finished.
const TAIL_MARKER: &str = "LAGTEST_DONE";

/// Paced bursts, so output is still flowing after the stall below. Each burst
/// is ~0.7 MB, well past the 256-frame broadcast window.
const BURST_CMD: &str = "for i in 1 2 3 4 5 6; do seq 1 100000; sleep 0.5; done; echo LAGTEST_DONE";

/// How long the consumer reads nothing at all.
const STALL: Duration = Duration::from_secs(2);

/// Ceiling on the post-stall drain; the assertion is convergence, not speed.
const CONVERGE_DEADLINE: Duration = Duration::from_secs(90);

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// Identity of one bootstrap generation on the wire.
type Generation = (TerminalId, StreamId, BootstrapId);

/// Per-generation live-sequence expectation, mirroring the client kernel's
/// `expect_next_seq`.
#[derive(Default)]
struct SequenceOracle {
    next: HashMap<Generation, u64>,
    /// Generations opened by a `BOOTSTRAP_BEGIN`. The opening bootstrap makes
    /// this 1, so a replacement published in answer to a gap is what takes it
    /// past that — which is why the assertion at the end reads `> 1`.
    generations: usize,
    /// Live frames seen, so the test knows the pump is running before it
    /// stalls.
    live_frames: usize,
}

impl SequenceOracle {
    fn open(&mut self, key: Generation, base_seq: u64) {
        self.next.insert(key, base_seq.saturating_add(1));
        self.generations += 1;
    }

    /// The client kernel's `expect_next_seq`, verbatim: a generation is opened
    /// by its bootstrap, and every live frame on it must be the exact next
    /// sequence. Output for a generation no bootstrap opened is not a case to
    /// accommodate — it is the `UnknownGeneration` the kernel rejects.
    fn observe(&mut self, key: &Generation, seq: u64) {
        self.live_frames += 1;
        let expected = self.next.get_mut(key).unwrap_or_else(|| {
            panic!(
                "TERMINAL_OUTPUT seq={seq} names a generation no BOOTSTRAP_BEGIN opened; \
                 ADR-0007 s4 requires the snapshot to precede every delta"
            )
        });
        assert_eq!(
            seq, *expected,
            "live sequence gap at {seq}; expected {expected} — the session kernel \
             rejects this frame and the consumer detaches with a protocol error",
        );
        *expected = seq.saturating_add(1);
    }
}

/// What one drained frame meant to the consumer.
enum Applied {
    /// The server ended the subscription instead of resyncing.
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

/// Spawn the bursting pane on the session-attached `owner` connection.
///
/// A second pane rather than the seed pane, so the seed keeps the session
/// alive for the whole run and the burst pane's exit cannot trip the
/// last-pane self-exit while the watcher is still reading.
async fn spawn_burst_pane(owner: &mut UnixStream) -> TerminalId {
    send_frame(
        owner,
        &FrameKind::SpawnTerminal {
            request_id: 1,
            group: GroupId::new(1),
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                BURST_CMD.to_owned(),
            ]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
        },
    )
    .await;
    loop {
        let (_type_byte, frame) = recv_typed(owner).await;
        if let FrameKind::TerminalSpawned { request_id, result } = frame
            && request_id == 1
        {
            match result {
                SpawnResult::Ok(id) => return id,
                other => panic!("SPAWN_TERMINAL failed: {other:?}"),
            }
        }
    }
}

/// Subscribe `watcher` to `pane` with `ATTACH_TERMINAL` and nothing else — no
/// session-scoped `ATTACH` on this connection, ever. That is the shape `phux
/// rec` and the FFI consumers take.
async fn attach_terminal_only(watcher: &mut UnixStream, pane: &TerminalId) -> Vec<FrameKind> {
    send_frame(
        watcher,
        &FrameKind::Command {
            request_id: 100,
            command: Command::AttachTerminal {
                terminal_id: pane.clone(),
            },
        },
    )
    .await;
    // Every frame the server interleaves ahead of the answer is *returned*,
    // never dropped. The bootstrap arrives on this connection before the
    // `COMMAND_RESULT` does, so a loop that discarded everything but the
    // result would swallow the whole opening generation and leave the caller
    // believing the stream started with a bare delta.
    let mut interleaved = Vec::new();
    loop {
        let (_type_byte, frame) = recv_typed(watcher).await;
        if let FrameKind::CommandResult { request_id, result } = &frame
            && *request_id == 100
        {
            assert!(
                matches!(result, CommandResult::Ok),
                "ATTACH_TERMINAL must succeed, got {result:?}",
            );
            return interleaved;
        }
        interleaved.push(frame);
    }
}

#[test]
fn lagged_attach_terminal_consumer_converges_on_a_replacement_generation() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.args(["-c", "while :; do sleep 3600; done"]);
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "lag", seed);

        let mut owner = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut owner, &attach_by_name("lag")).await;
        let pane = spawn_burst_pane(&mut owner).await;

        let mut watcher = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let opening = attach_terminal_only(&mut watcher, &pane).await;

        let mut oracle = SequenceOracle::default();
        let mut screen = Screen::new(COLS, ROWS).expect("screen oracle");

        // The opening generation arrives interleaved ahead of the
        // `COMMAND_RESULT`, so it is folded in here rather than read off the
        // socket below.
        for frame in &opening {
            if let Applied::Fatal(what) = apply(frame, &mut oracle, &mut screen) {
                panic!("subscription failed while opening: {what}");
            }
        }
        assert!(
            oracle.generations >= 1,
            "ATTACH_TERMINAL must deliver a bootstrap before any delta \
             (ADR-0007 s4); got {opening:?}",
        );

        // Drain until the pane is actually streaming, so the pump is live
        // before the stall.
        while oracle.live_frames == 0 {
            let (_type_byte, frame) = recv_typed(&mut watcher).await;
            if let Applied::Fatal(what) = apply(&frame, &mut oracle, &mut screen) {
                panic!("subscription died before the stall: {what}");
            }
        }

        // The stall. Nothing is read from the watcher's socket, so its writer
        // task blocks, its mailbox fills, and its pump falls off the broadcast.
        tokio::time::sleep(STALL).await;

        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < CONVERGE_DEADLINE,
                "ATTACH_TERMINAL consumer never converged after the broadcast gap",
            );
            let (_type_byte, frame) = recv_typed(&mut watcher).await;
            if let Applied::Fatal(what) = apply(&frame, &mut oracle, &mut screen) {
                panic!("server ended the subscription instead of resyncing: {what}");
            }
            if screen.contains(TAIL_MARKER) {
                break;
            }
        }

        assert!(
            oracle.generations > 1,
            "consumer converged without a replacement generation ever being published, \
             so this run never actually lagged — the test proves nothing",
        );

        drop(watcher);
        drop(owner);
        let _ = shutdown.send(());
        let _ = server.await;
    });
}
