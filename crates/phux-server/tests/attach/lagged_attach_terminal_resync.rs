//! An `ATTACH_RESOURCE` consumer that falls behind the broadcast ring must
//! converge on a replacement generation, never see a live `seq` gap (which
//! the client kernel rejects as a protocol error). This pump is separate from
//! the session `ATTACH` pump that `terminal::lagged_consumer_resync` covers.
//!
//! The lag is driven, not timed: the pane dumps only once a gate opens, and
//! the watcher is not read until the server's own grid shows the dump's end,
//! by which point a 4-slot ring has certainly been lapped. The bootstrap
//! arrives interleaved ahead of the `ATTACH_RESOURCE` reply, so it is kept.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, ResourceLifecycle, SpawnResource, SpawnResult,
    StateScope,
};
use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, ServerHandles, Spawn, attach_by_name, command, recv_typed, run_local,
    send_frame, spawn_resource, spawn_server_with_seed_cmd, wait_for_server_screen_text,
    wait_for_socket,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

use super::common::sh;

const TAIL_MARKER: &str = "LAGTEST_DONE";
/// ~6.9 MB, far beyond what the stalled consumer can buffer.
const DUMP_LINES: u32 = 1_000_000;
const TEST_OUTPUT_BROADCAST: usize = 4;
/// A hang guard only; every wait ends on a specific frame or screen state.
const HANG_GUARD: Duration = Duration::from_secs(60);

type Generation = (ResourceId, StreamId, BootstrapId);

/// The client kernel's `expect_next_seq`: every generation is opened by its
/// bootstrap and every live frame on it is the exact next sequence.
#[derive(Default)]
struct Oracle {
    next: HashMap<Generation, u64>,
    generations: usize,
}

impl Oracle {
    /// Fold one frame into the oracle and `screen`; panics on a gap or on
    /// the server ending the subscription instead of resyncing.
    fn apply(&mut self, frame: &FrameKind, screen: &mut Screen) {
        match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                base_seq,
                ..
            } => {
                self.next.insert(
                    (terminal_id.clone(), *stream_id, *bootstrap_id),
                    base_seq + 1,
                );
                self.generations += 1;
            }
            FrameKind::BootstrapChunk { payload, .. } => screen.write(payload),
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => {
                let key = (terminal_id.clone(), *stream_id, *bootstrap_id);
                let expected = self
                    .next
                    .get_mut(&key)
                    .unwrap_or_else(|| panic!("seq={seq} for a generation no bootstrap opened"));
                assert_eq!(*seq, *expected, "live sequence gap");
                *expected = seq + 1;
                screen.write(bytes);
            }
            FrameKind::Detached { .. } | FrameKind::Error { .. } => {
                panic!("server ended the subscription instead of resyncing: {frame:?}")
            }
            _ => {}
        }
    }
}

struct Lagged {
    _tmp: TempDir,
    _server: ServerHandles,
    owner: UnixStream,
    watcher: UnixStream,
    probe: UnixStream,
    pane: ResourceId,
    oracle: Oracle,
    screen: Screen,
}

/// Spawn a burst pane (`exit_after` or stay alive), subscribe a watcher with
/// `ATTACH_RESOURCE` only, open the gate, and wait until the dump is on the
/// server's grid while the watcher sits unread.
async fn lagged(exit_after: bool, retained: bool) -> Lagged {
    phux_server::resource::set_output_broadcast_capacity_for_test(TEST_OUTPUT_BROADCAST);
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("phux.sock");
    let gate = tmp.path().join("dump.gate");
    let server =
        spawn_server_with_seed_cmd(socket.clone(), "lag", sh("while :; do sleep 3600; done"));

    let mut owner = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut owner, &attach_by_name("lag")).await;
    let tail = if exit_after { "" } else { "; exec sleep 3600" };
    let script = format!(
        "while [ ! -e '{}' ]; do sleep 0.02; done; seq 1 {DUMP_LINES}; echo {TAIL_MARKER}{tail}",
        gate.display()
    );
    let spawn = Spawn {
        resource: retained.then(|| Box::new(SpawnResource::default().with_retain_secs(Some(600)))),
        ..Spawn::command(&["/bin/sh", "-c", &script])
    };
    let SpawnResult::Ok(pane) = spawn_resource(&mut owner, 1, spawn).await else {
        panic!("burst pane spawn failed");
    };

    let mut watcher = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
    let attach = FrameKind::Command {
        request_id: 100,
        command: Command::AttachResource {
            terminal_id: pane.clone(),
            role_policy: None,
        },
    };
    send_frame(&mut watcher, &attach).await;
    let mut oracle = Oracle::default();
    let mut screen = Screen::new(80, 24).unwrap();
    loop {
        let (_, frame) = recv_typed(&mut watcher).await;
        if let FrameKind::CommandResult {
            request_id: 100,
            result,
        } = &frame
        {
            assert_eq!(*result, CommandResult::Ok);
            break;
        }
        oracle.apply(&frame, &mut screen);
    }
    assert!(
        oracle.generations >= 1,
        "bootstrap must precede any delta (ADR-0007 s4)"
    );

    let mut probe = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
    std::fs::write(&gate, b"").unwrap();
    // A non-retained exiting pane is reaped, so its screen cannot be polled.
    if !exit_after || retained {
        wait_for_server_screen_text(&mut probe, &pane, TAIL_MARKER, HANG_GUARD).await;
    }
    Lagged {
        _tmp: tmp,
        _server: server,
        owner,
        watcher,
        probe,
        pane,
        oracle,
        screen,
    }
}

/// Poll `GET_STATE` until `pred` holds for the pane's listing (`None` = gone).
async fn wait_for_pane(
    probe: &mut UnixStream,
    pane: &ResourceId,
    pred: impl Fn(Option<ResourceLifecycle>) -> bool,
) {
    let started = Instant::now();
    for request_id in 500.. {
        assert!(
            started.elapsed() < HANG_GUARD,
            "pane never reached the expected state"
        );
        let get_state = Command::GetState {
            scope: StateScope::Server,
        };
        let CommandResult::OkWith(CommandValue::State(snapshot)) =
            command(probe, request_id, get_state).await
        else {
            panic!("GET_STATE failed");
        };
        let lifecycle = snapshot
            .resources
            .iter()
            .find(|r| &r.id == pane)
            .map(|r| r.lifecycle);
        if pred(lifecycle) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl Lagged {
    /// Drain the watcher until the tail marker renders.
    async fn converge(&mut self, forbid_close: bool) {
        let started = Instant::now();
        while !self.screen.contains(TAIL_MARKER) {
            assert!(
                started.elapsed() < HANG_GUARD,
                "lagged consumer never converged"
            );
            let (_, frame) = recv_typed(&mut self.watcher).await;
            assert!(
                !(forbid_close && matches!(frame, FrameKind::ResourceClosed { .. })),
                "a retained pane is not closed at exit"
            );
            self.oracle.apply(&frame, &mut self.screen);
        }
        self.assert_lagged();
    }

    fn assert_lagged(&self) {
        assert!(
            self.oracle.generations > 1,
            "the consumer never lagged; the test proves nothing"
        );
    }
}

#[test]
fn lagged_attach_terminal_consumer_converges_on_a_replacement_generation() {
    run_local(async {
        lagged(false, false).await.converge(false).await;
    });
}

/// ADR-0124: a retained pane keeps its engine after exit, so the resync a
/// lagged pump is owed is still published.
#[test]
fn lagged_consumer_of_a_retained_pane_converges_on_the_final_screen_after_exit() {
    run_local(async {
        let mut lag = lagged(true, true).await;
        wait_for_pane(&mut lag.probe, &lag.pane, |l| {
            l == Some(ResourceLifecycle::Exited)
        })
        .await;
        lag.converge(true).await;
    });
}

/// phux-fpgl.28: a non-retained pane flushes the final screen to fenced pumps
/// before `RESOURCE_CLOSED`. The owner is drained so only the watcher lags.
#[test]
fn lagged_consumer_of_an_exiting_pane_receives_the_final_screen_before_close() {
    run_local(async {
        let mut lag = lagged(true, false).await;
        tokio::select! {
            biased;
            () = wait_for_pane(&mut lag.probe, &lag.pane, |l| l.is_none()) => {}
            () = async { loop { recv_typed(&mut lag.owner).await; } } => {}
        }
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < HANG_GUARD,
                "never converged after close"
            );
            let (_, frame) = recv_typed(&mut lag.watcher).await;
            if matches!(&frame, FrameKind::ResourceClosed { terminal_id, .. } if *terminal_id == lag.pane)
            {
                assert!(
                    lag.screen.contains(TAIL_MARKER),
                    "closed without the final screen"
                );
                break;
            }
            lag.oracle.apply(&frame, &mut lag.screen);
        }
        lag.assert_lagged();
    });
}
