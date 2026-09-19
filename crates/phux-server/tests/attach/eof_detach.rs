//! Last-shell natural exit keeps the Terminal live (`phux-bnbd`).
//!
//! Typing `exit` in the only remaining shell used to broadcast
//! `RESOURCE_CLOSED` and reap the session. Clients then either froze
//! (pre-phux-it8) or detached (phux-4r1 consumer policy). The server now
//! replaces the child in place: same Terminal id, a fresh default shell,
//! a resync of the new grid. The server MUST NOT send `DETACHED` or
//! `RESOURCE_CLOSED` for that last natural exit.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, ResourceLifecycle, StateScope, TYPE_ATTACHED,
    TYPE_BOOTSTRAP_BEGIN, TYPE_DETACHED, TYPE_RESOURCE_CLOSED,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, join_after_shutdown,
    recv_command_result, recv_typed, recv_until_deadline, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// A shell that outlives the `ATTACH` handshake and then exits with code
/// `0` **when the test says so**, by waiting for `release` to appear.
///
/// We want the child to outlive the handshake — otherwise we're
/// asserting on a race we don't care about ("client never even received
/// the snapshot because the actor was already gone"), not the lifecycle
/// we want to pin down ("server reports the pane died, with its exit
/// status, when the PTY exits").
///
/// This was `sleep 0.2; exit 0`, whose doc claimed the value was not
/// load-bearing. It was: under parallel test load the attach lost the race,
/// the pane had already exited, its session was reaped, and `ATTACH` came
/// back as `ERROR` rather than `ATTACHED` (phux-w266, ~1 in 5). A barrier
/// expresses the same intent without a number to be wrong about, and makes
/// the EOF fire at a point the test chooses rather than one it hopes for.
/// `sleep` and `[ -f ]` are POSIX so no path probing is needed.
fn pick_true_command(release: &std::path::Path, exited: &std::path::Path) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(format!(
        "until [ -f '{}' ]; do sleep 0.01; done; echo done > '{}'; exit 0",
        release.display(),
        exited.display()
    ));
    cmd
}

/// Drain inbound frames until `deadline`, failing if `RESOURCE_CLOSED` or
/// `DETACHED` arrives. Last-shell replacement must keep the attach alive.
async fn assert_no_close(stream: &mut UnixStream, deadline: Duration) {
    let end = tokio::time::Instant::now() + deadline;
    let closed = recv_until_deadline(stream, end, |type_byte, _frame| {
        assert_ne!(
            type_byte, TYPE_DETACHED,
            "server must NOT send DETACHED on last-shell exit",
        );
        assert_ne!(
            type_byte, TYPE_RESOURCE_CLOSED,
            "last-shell natural exit must replace the child, not close the Terminal",
        );
        None::<()>
    })
    .await;
    assert!(
        closed.is_none(),
        "last-shell natural exit must not close the Terminal"
    );
}

/// Last-shell PTY EOF replaces the child in place. The attached client
/// must not see `RESOURCE_CLOSED` or `DETACHED`, and `GET_STATE` still
/// names a live Terminal.
#[test]
fn last_shell_eof_keeps_the_terminal_live() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // The seed shell exits with code 0 once this test releases it → PTY
        // EOF lands in the actor. Releasing it after ATTACHED is what makes
        // "the EOF watcher fires while the client is still draining" a fact
        // rather than a hope.
        let release = tmp.path().join("release");
        let exited = tmp.path().join("exited");
        let cmd = pick_true_command(&release, &exited);
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

        // The handshake has landed, so the pane may now die: everything below
        // is the lifecycle this test exists to pin.
        std::fs::write(&release, b"go").expect("release the seed pane");
        let wait_exit = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        while !exited.exists() {
            assert!(
                tokio::time::Instant::now() < wait_exit,
                "seed pane never recorded its exit"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // ---- TERMINAL_SNAPSHOT (one per pane in focused window) ----
        let (type_byte, _snap_frame) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_BOOTSTRAP_BEGIN,
            "second server-to-client frame must be TERMINAL_SNAPSHOT",
        );

        assert_no_close(&mut stream, Duration::from_millis(250)).await;

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 10,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        let result = recv_command_result(&mut stream, 10).await;
        let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
            panic!("GET_STATE failed: {result:?}");
        };
        assert!(
            snapshot.resources.iter().any(|resource| {
                resource.lifecycle == ResourceLifecycle::Running && resource.exit.is_none()
            }),
            "last-shell exit must leave a live Terminal, got {snapshot:?}"
        );

        drop(stream);
        join_after_shutdown(shutdown_tx, server_handle).await;
    });
}
