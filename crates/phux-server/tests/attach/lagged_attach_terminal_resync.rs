//! phux-l96p.10, second pump: an `ATTACH_RESOURCE` consumer that falls behind
//! must converge, exactly like a session-attached one.
//!
//! `ATTACH_RESOURCE` has its own output pump (`runtime::commands`), separate
//! from the ATTACH pump in `runtime::attach`. When the broadcast gap fence was
//! added it went into one of them, and this is the path that did not get it:
//! `phux rec`, `phux play`, headless pane watchers, the FFI and mobile
//! consumers, and a federation hub's proxy subscription all arrive here. Its
//! lag handler asked the actor for a resync and then resumed forwarding live
//! deltas immediately, putting a `RESOURCE_OUTPUT` whose `seq` skips the
//! dropped window on the wire — which the consumer's session kernel rejects as
//! a protocol error, killing the consumer before the resync can land.
//!
//! The assertions mirror `lagged_consumer_resync.rs` frame for frame, because
//! the guarantee is the same guarantee; only the subscription verb differs.
//! Before the fix this fails on the first one, seconds into the drain.
//!
//! The lag is driven, not timed (phux-8kpb). The pane dumps only once the test
//! opens a gate, and the test stops reading until the server's own grid shows
//! the dump's last line. By then every frame of a dump far larger than the
//! consumer's buffering has been published onto a ring of
//! [`TEST_OUTPUT_BROADCAST`] slots, so the stalled pump has certainly been
//! lapped. The pane then stays alive and quiet, so the resync the pump asks
//! for is always answered and never overwritten. The previous shape — paced
//! bursts against a fixed two-second stall, with the pane exiting straight
//! after its marker — made both halves a race under CPU load: a slow runner
//! could reach the end of the stall before the ring overflowed, or see the
//! pane exit (and its resync die with it) while the pump was still fenced,
//! leaving the consumer waiting on a frame that never came.
//!
//! One trap this test fell into once, worth naming: the bootstrap arrives
//! *interleaved ahead of* the `COMMAND_RESULT` that answers `ATTACH_RESOURCE`,
//! so a helper that loops discarding everything but the result swallows the
//! whole opening generation, and the stream then looks like it began with a
//! bare `RESOURCE_OUTPUT`. It does not: ADR-0007 s4 requires the snapshot to
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
use std::path::Path;
use std::time::{Duration, Instant};

use phux_protocol::ids::{BootstrapId, GroupId, ResourceId, StreamId};
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, SpawnResult};
use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, recv_until, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_server_screen_text, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

/// What the bursting pane prints when it is finished.
const TAIL_MARKER: &str = "LAGTEST_DONE";

/// Lines the pane dumps once the gate opens: ~6.9 MB. The stalled consumer
/// can soak up at most its mailbox, the writer's coalesced batch and the
/// socket buffers — about 2 MB of the actor's <= 48 KiB frames — so the rest
/// of the dump laps the ring many times over, however the reader coalesces.
const DUMP_LINES: u32 = 1_000_000;

/// Broadcast ring used by this test. Four slots overflow as soon as the
/// stalled pump is a handful of coalesced frames behind — the same shape as
/// production `Lagged`, without depending on a 12 MiB dump past the 256-slot
/// window.
const TEST_OUTPUT_BROADCAST: usize = 4;

/// Bound on each event-driven wait below. A hang guard, never a timing
/// assertion: every wait ends on a specific frame or screen state.
const HANG_GUARD: Duration = Duration::from_secs(60);

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// Identity of one bootstrap generation on the wire.
type Generation = (ResourceId, StreamId, BootstrapId);

/// The pane's workload: wait for `gate`, dump, print the marker, then stay
/// alive. Staying alive is load-bearing: a pane that exits is reaped, and a
/// resync still owed to a fenced pump dies with it.
fn burst_cmd(gate: &Path) -> String {
    format!(
        "while [ ! -e '{}' ]; do sleep 0.02; done; seq 1 {DUMP_LINES}; \
         echo {TAIL_MARKER}; exec sleep 3600",
        gate.display(),
    )
}

/// The same workload for a pane retained after its exit (ADR-0124): wait for
/// `gate`, dump, print the marker, and exit at once. Retention keeps the
/// engine, so the resync a fenced pump is owed still lands after the exit
/// (phux-fpgl.28).
fn exiting_burst_cmd(gate: &Path) -> String {
    format!(
        "while [ ! -e '{}' ]; do sleep 0.02; done; seq 1 {DUMP_LINES}; echo {TAIL_MARKER}",
        gate.display(),
    )
}

/// Per-generation live-sequence expectation, mirroring the client kernel's
/// `expect_next_seq`.
#[derive(Default)]
struct SequenceOracle {
    next: HashMap<Generation, u64>,
    /// Generations opened by a `BOOTSTRAP_BEGIN`. The opening bootstrap makes
    /// this 1, so a replacement published in answer to a gap is what takes it
    /// past that — which is why the assertion at the end reads `> 1`.
    generations: usize,
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
        let expected = self.next.get_mut(key).unwrap_or_else(|| {
            panic!(
                "RESOURCE_OUTPUT seq={seq} names a generation no BOOTSTRAP_BEGIN opened; \
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

/// Spawn the bursting pane on the session-attached `owner` connection.
///
/// A second pane rather than the seed pane, so the seed keeps the session
/// alive for the whole run independently of the burst pane.
async fn spawn_burst_pane(
    owner: &mut UnixStream,
    cmd: String,
    resource: Option<Box<phux_protocol::wire::frame::SpawnResource>>,
) -> ResourceId {
    send_frame(
        owner,
        &FrameKind::SpawnResource {
            request_id: 1,
            group: GroupId::new(1),
            command: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), cmd]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource,
        },
    )
    .await;
    recv_until(owner, |_, frame| match frame {
        FrameKind::ResourceSpawned { request_id, result } if request_id == 1 => match result {
            SpawnResult::Ok(id) => Some(id),
            other => panic!("SPAWN_RESOURCE failed: {other:?}"),
        },
        _ => None,
    })
    .await
}

/// Subscribe `watcher` to `pane` with `ATTACH_RESOURCE` and nothing else — no
/// session-scoped `ATTACH` on this connection, ever. That is the shape `phux
/// rec` and the FFI consumers take.
async fn attach_terminal_only(watcher: &mut UnixStream, pane: &ResourceId) -> Vec<FrameKind> {
    send_frame(
        watcher,
        &FrameKind::Command {
            request_id: 100,
            command: Command::AttachResource {
                terminal_id: pane.clone(),
                role_policy: None,
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
                "ATTACH_RESOURCE must succeed, got {result:?}",
            );
            return interleaved;
        }
        interleaved.push(frame);
    }
}

#[test]
fn lagged_attach_terminal_consumer_converges_on_a_replacement_generation() {
    phux_server::resource::set_output_broadcast_capacity_for_test(TEST_OUTPUT_BROADCAST);
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let gate = tmp.path().join("dump.gate");
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.args(["-c", "while :; do sleep 3600; done"]);
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "lag", seed);

        let mut owner = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut owner, &attach_by_name("lag")).await;
        let pane = spawn_burst_pane(&mut owner, burst_cmd(&gate), None).await;

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
            "ATTACH_RESOURCE must deliver a bootstrap before any delta \
             (ADR-0007 s4); got {opening:?}",
        );

        // The stall, driven rather than timed. Nothing is read from the
        // watcher's socket from here until the pane's server-side grid shows
        // the end of the dump, so its writer blocks, its mailbox fills, and
        // the rest of the dump laps the [`TEST_OUTPUT_BROADCAST`]-slot ring.
        // The probe carries no subscription, so polling it unblocks nothing.
        let mut probe = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        std::fs::write(&gate, b"").expect("open the dump gate");
        wait_for_server_screen_text(&mut probe, &pane, TAIL_MARKER, HANG_GUARD).await;

        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < HANG_GUARD,
                "ATTACH_RESOURCE consumer never converged after the broadcast gap",
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

        drop(probe);
        drop(watcher);
        drop(owner);
        let _ = shutdown.send(());
        let _ = server.await;
    });
}

/// Poll `GET_STATE` on `probe` until `pane` is listed as exited (ADR-0124).
async fn wait_until_exited(probe: &mut UnixStream, pane: &ResourceId) {
    use phux_protocol::wire::frame::{CommandValue, ResourceLifecycle, StateScope};
    let started = Instant::now();
    for request_id in 500.. {
        assert!(
            started.elapsed() < HANG_GUARD,
            "the retained pane never exited"
        );
        send_frame(
            probe,
            &FrameKind::Command {
                request_id,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        let result = phux_server_testkit::await_command_result(probe, request_id).await;
        let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
            panic!("GET_STATE failed: {result:?}");
        };
        let exited = snapshot
            .resources
            .iter()
            .any(|r| &r.id == pane && matches!(r.lifecycle, ResourceLifecycle::Exited));
        if exited {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// phux-fpgl.28 with ADR-0124: a fenced consumer of a RETAINED pane whose
/// child exits while the consumer is behind still converges on the final
/// screen. The pane keeps its engine after the exit, so the replacement
/// generation the lagged pump is owed is still published, and the consumer
/// is not left on a subscription that ends in `RESOURCE_CLOSED` alone.
#[test]
fn lagged_consumer_of_a_retained_pane_converges_on_the_final_screen_after_exit() {
    phux_server::resource::set_output_broadcast_capacity_for_test(TEST_OUTPUT_BROADCAST);
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let gate = tmp.path().join("dump.gate");
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.args(["-c", "while :; do sleep 3600; done"]);
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "lag", seed);

        let mut owner = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut owner, &attach_by_name("lag")).await;
        let retained =
            phux_protocol::wire::frame::SpawnResource::default().with_retain_secs(Some(600));
        let pane = spawn_burst_pane(
            &mut owner,
            exiting_burst_cmd(&gate),
            Some(Box::new(retained)),
        )
        .await;

        let mut watcher = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let opening = attach_terminal_only(&mut watcher, &pane).await;
        let mut oracle = SequenceOracle::default();
        let mut screen = Screen::new(COLS, ROWS).expect("screen oracle");
        for frame in &opening {
            if let Applied::Fatal(what) = apply(frame, &mut oracle, &mut screen) {
                panic!("subscription failed while opening: {what}");
            }
        }

        // The stall, as above, except that the pane now exits right after
        // its marker, and the consumer is still unread when it does.
        let mut probe = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        std::fs::write(&gate, b"").expect("open the dump gate");
        wait_for_server_screen_text(&mut probe, &pane, TAIL_MARKER, HANG_GUARD).await;
        wait_until_exited(&mut probe, &pane).await;

        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < HANG_GUARD,
                "a lagged consumer of a retained pane never converged after its exit",
            );
            let (_type_byte, frame) = recv_typed(&mut watcher).await;
            assert!(
                !matches!(&frame, FrameKind::ResourceClosed { terminal_id, .. } if *terminal_id == pane),
                "a retained pane is not closed at exit",
            );
            if let Applied::Fatal(what) = apply(&frame, &mut oracle, &mut screen) {
                panic!("server ended the subscription instead of resyncing: {what}");
            }
            if screen.contains(TAIL_MARKER) {
                break;
            }
        }
        assert!(
            oracle.generations > 1,
            "the consumer never lagged, so the test proves nothing",
        );

        drop(probe);
        drop(watcher);
        drop(owner);
        let _ = shutdown.send(());
        let _ = server.await;
    });
}
