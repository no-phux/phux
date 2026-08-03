//! Wire-level integration test for the PTY-EOF → `TERMINAL_CLOSED` path
//! (`phux-it8`, reshaped by `phux-4r1`).
//!
//! Reproduces the user-visible "type `exit` in the inner shell and the
//! client freezes forever in alt-screen" bug. Before the original fix,
//! the `TerminalActor`'s EOF branch just dropped its PTY receiver and
//! kept the actor alive "for snapshot/input drain" — but neither the
//! runtime nor any attached client got told the pane was dead, so the
//! client sat in its `tokio::select!` waiting for frames that never came.
//!
//! `phux-it8` first closed that hole by having the server send
//! `FrameKind::Detached` on EOF. `phux-4r1` then reshaped that EOF
//! signal into the L1 lifecycle event `FrameKind::TerminalClosed`
//! (ADR-0015 L1): the server now reports the *fact* that the PTY exited
//! (carrying its exit status) and stops deciding detach. The
//! "no Terminals left in my collection ⇒ detach" policy moved out of
//! the server runtime and into the TUI consumer (`attach::driver`).
//!
//! This test pins down the server half of that contract from the wire's
//! point of view: pre-seed with a shell that exits promptly, attach a
//! client, and assert a `TERMINAL_CLOSED { exit_status: Some(0) }` frame
//! arrives at all. The server MUST NOT send `DETACHED` on EOF anymore.
//!
//! What is asserted is arrival, not latency: the drain below is bounded by
//! the shared `WIRE_RECV_TIMEOUT` so a genuinely hung server still
//! fails the run. See the call site for why the old hand-picked two-second
//! bound was a flake (phux-br1f).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_DETACHED, TYPE_TERMINAL_CLOSED,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// A shell that lives long enough for the test client to complete
/// `ATTACH` + receive `TERMINAL_SNAPSHOT` (the existing wire contract)
/// BEFORE the EOF fires, then exits with code `0`.
///
/// We want the child to outlive the handshake — otherwise we're
/// asserting on a race we don't care about ("client never even received
/// the snapshot because the actor was already gone"), not the lifecycle
/// we want to pin down ("server reports the pane died, with its exit
/// status, when the PTY exits").
///
/// 200ms is a generous floor against a slow CI scheduler while still
/// being well below the test's drain deadline. The exact value isn't
/// load-bearing; anything from ~50ms up works. `sleep` is POSIX so no
/// path probing needed.
fn pick_true_command() -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("sleep 0.2; exit 0");
    cmd
}

/// Drain non-`TERMINAL_CLOSED` frames (`ATTACHED`, `TERMINAL_SNAPSHOT`,
/// late `TERMINAL_OUTPUT`) until a `TERMINAL_CLOSED` frame arrives or
/// `deadline` elapses. Returns the decoded frame on success.
///
/// We can't just `recv_typed` once and assert: between `ATTACHED` and
/// the EOF-driven `TERMINAL_CLOSED`, the runtime ships one
/// `TERMINAL_SNAPSHOT` per pane, and the `TerminalActor`'s PTY pump may
/// emit a few stray `TERMINAL_OUTPUT` chunks from libghostty's snapshot
/// replay before the EOF fires. Skip those rather than fail on them.
///
/// A `DETACHED` frame during the drain is a hard failure: under
/// `phux-4r1` the server must NOT send `DETACHED` on PTY EOF — that
/// policy moved to the consumer.
async fn await_terminal_closed(stream: &mut UnixStream, deadline: Duration) -> Option<FrameKind> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            return None;
        };
        assert_ne!(
            type_byte, TYPE_DETACHED,
            "server must NOT send DETACHED on PTY EOF (phux-4r1: detach is consumer policy)",
        );
        if type_byte == TYPE_TERMINAL_CLOSED {
            assert!(
                matches!(frame, FrameKind::TerminalClosed { .. }),
                "TYPE_TERMINAL_CLOSED must decode to FrameKind::TerminalClosed",
            );
            return Some(frame);
        }
    }
}

/// `TerminalActor` PTY EOF (from the seed shell exiting with code 0)
/// drives the runtime to broadcast `FrameKind::TerminalClosed` — the L1
/// lifecycle event — to the attached client, carrying the exit status.
///
/// Before phux-it8, the client would never receive any post-snapshot
/// frame and this test would time out — exactly the user-facing "client
/// freezes in alt-screen" symptom. Before phux-4r1 the server reported
/// the death by sending `DETACHED`; now it sends the structured
/// `TERMINAL_CLOSED` and leaves the detach decision to the consumer.
#[test]
fn pty_eof_drives_terminal_closed_to_attached_client() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // The seed shell exits with code 0 after a short sleep → PTY EOF
        // lands in the actor, guaranteeing the EOF watcher fires while the
        // client is still draining.
        let cmd = pick_true_command();
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "demo", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ---- ATTACH ----
        send_frame(&mut stream, &attach_by_name("demo")).await;

        // ---- ATTACHED ----
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED",
        );

        // ---- TERMINAL_SNAPSHOT (one per pane in focused window) ----
        let (type_byte, _snap_frame) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_BOOTSTRAP_BEGIN,
            "second server-to-client frame must be TERMINAL_SNAPSHOT",
        );

        // ---- TERMINAL_CLOSED (the contract under test) ----
        //
        // The bound is the shared `WIRE_RECV_TIMEOUT` rather than a number
        // picked for this test, and the number is not load-bearing: what is
        // asserted is that the frame arrives and carries the exit status,
        // never how quickly. Arrival is sub-100ms locally.
        //
        // It used to be a hand-picked two seconds, described in this very
        // comment as "generous". Two seconds is generous on an idle laptop
        // and not generous on a loaded runner — this test failed at 79.9s in
        // a saturated full-workspace run and passed in 0.847s re-run alone on
        // the same tree (phux-br1f). A deadline that measures the machine
        // rather than the server is a latent flake, and a suite that cries
        // wolf gets ignored, which is how a real regression ships.
        //
        // Raising it does not weaken the contract. A server that never sends
        // TERMINAL_CLOSED — the pre-phux-it8 "client freezes in alt-screen"
        // regression — still fails, just at the 15s ceiling instead of 2s,
        // with the same message.
        let closed = await_terminal_closed(&mut stream, WIRE_RECV_TIMEOUT).await;
        let closed = closed
            .expect("client must receive FrameKind::TerminalClosed after the seed shell's PTY EOF");
        match closed {
            FrameKind::TerminalClosed { exit_status, .. } => {
                assert_eq!(
                    exit_status,
                    Some(0),
                    "the seed shell exited with code 0; TERMINAL_CLOSED must carry it",
                );
            }
            other => panic!("expected TerminalClosed, got {other:?}"),
        }

        // Clean teardown. The seed shell was the server's only pane, so
        // its exit triggers the server self-exit path (phux-60s); the
        // join below confirms the server stopped on its own. The
        // explicit shutdown signal is a belt-and-suspenders no-op if the
        // server already exited.
        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
