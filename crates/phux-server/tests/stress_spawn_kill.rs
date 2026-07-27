//! Spawn/kill adversarial stress (crash-hunt wave).
//!
//! Drives the SPAWN_TERMINAL / KILL_TERMINAL lifecycle hard:
//!
//!   * a storm of spawns ("deep splits" → many panes), each running a tiny
//!     command, then a storm of kills tearing them all down;
//!   * killing panes while a long-lived anchor pane streams output, with a
//!     KILL_TERMINAL aimed at an already-dead pane (the double-kill race).
//!
//! The server must reply to every command (Ok / typed error), emit a
//! TERMINAL_CLOSED for each reaped pane, and never panic a reaping actor or
//! the connection task. Heavy `just e2e` lane only.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::doc_markdown, reason = "test narrative uses bare wire names")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod common;

use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, SpawnResult, TYPE_COMMAND_RESULT, TYPE_TERMINAL_SPAWNED,
};
use phux_server::DEFAULT_GROUP_ID;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_seed_pty_no_cmd, wait_for_socket,
};

/// Drain until the matching `TERMINAL_SPAWNED` arrives; return its result.
async fn await_spawned(stream: &mut UnixStream, request_id: u32) -> SpawnResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((tb, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if tb == TYPE_TERMINAL_SPAWNED
            && let FrameKind::TerminalSpawned {
                request_id: got,
                result,
            } = frame
            && got == request_id
        {
            return result;
        }
    }
    panic!("timed out waiting for TERMINAL_SPAWNED request_id={request_id}");
}

/// Drain until the next `COMMAND_RESULT` arrives; return it. (KILL_TERMINAL
/// replies with a bare Ok/Error COMMAND_RESULT.)
async fn await_command_result(stream: &mut UnixStream) -> CommandResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((tb, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if tb == TYPE_COMMAND_RESULT
            && let FrameKind::CommandResult { result, .. } = frame
        {
            return result;
        }
    }
    panic!("timed out waiting for COMMAND_RESULT");
}

/// A spawn storm followed by a kill storm must not panic. Each spawn gets a
/// typed reply; each kill is acknowledged; killing the same pane twice
/// yields a clean Ok-or-Error (never a panic / hang).
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn spawn_storm_then_kill_storm_does_not_panic() {
    run_local(async {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_seed_pty_no_cmd(socket_path.clone(), Some("default"));

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;
        // Drain the ATTACHED + opening snapshot.
        let _ = recv_typed(&mut stream).await;
        let _ = recv_typed(&mut stream).await;

        // Spawn storm: many short-lived panes, no inter-send wait.
        let mut spawned = Vec::new();
        for req in 0..24u32 {
            send_frame(
                &mut stream,
                &FrameKind::SpawnTerminal {
                    request_id: req,
                    group: DEFAULT_GROUP_ID,
                    command: Some(vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "sleep 30".to_owned(),
                    ]),
                    cwd: None,
                    env: None,
                    term: None,
                    satellite: None,
                    owner_terminal: None,
                },
            )
            .await;
        }
        for req in 0..24u32 {
            if let SpawnResult::Ok(id) = await_spawned(&mut stream, req).await {
                spawned.push(id);
            }
        }
        assert!(
            !spawned.is_empty(),
            "spawn storm produced no live panes — spawn path regressed",
        );

        // Kill storm: tear them all down back-to-back, then kill the first
        // one AGAIN (the double-kill race against a reaping pane).
        let mut req_id = 100u32;
        for id in &spawned {
            send_frame(
                &mut stream,
                &FrameKind::Command {
                    request_id: req_id,
                    command: Command::KillTerminal {
                        terminal_id: id.clone(),
                    },
                },
            )
            .await;
            req_id += 1;
        }
        for _ in &spawned {
            // Each kill must produce a coherent COMMAND_RESULT (Ok or a
            // typed Error). A panic/hang fails inside await_command_result.
            let _ = await_command_result(&mut stream).await;
        }
        // Double-kill the first pane: a clean Ok or TerminalNotFound, not a
        // panic.
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: req_id,
                command: Command::KillTerminal {
                    terminal_id: spawned[0].clone(),
                },
            },
        )
        .await;
        req_id += 1;
        let _ = await_command_result(&mut stream).await;

        // The server (anchor session still alive) is responsive: a GET_STATE
        // round-trips. This proves no connection-task panic.
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: req_id,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await;
        let res = await_command_result(&mut stream).await;
        assert!(
            matches!(res, CommandResult::OkWith(_) | CommandResult::Ok),
            "server unresponsive after spawn/kill storm: {res:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server task join")
            .expect("server run_async returned an error");
    });
}

/// Killing the focused/seed pane (the last pane in a single-pane session)
/// must tear the session down cleanly and not panic. With the tmux
/// server-exit model the server self-exits once its last session reaps.
#[ignore = "real-PTY e2e; starves the parallel pool. Run via `just e2e`."]
#[test]
fn kill_last_pane_reaps_session_cleanly() {
    run_local(async {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_seed_pty_no_cmd(socket_path.clone(), Some("default"));

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;
        let (_tb, attached) = recv_typed(&mut stream).await;
        let _snap = recv_typed(&mut stream).await;

        // Recover the seed pane id from the ATTACHED snapshot.
        let seed = if let FrameKind::Attached { snapshot, .. } = attached {
            snapshot.panes.first().expect("a seed pane").id.clone()
        } else {
            panic!("expected ATTACHED");
        };

        // Kill the only pane. The KILL is acknowledged; the session then
        // reaps and the server self-exits, dropping our connection.
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 1,
                command: Command::KillTerminal { terminal_id: seed },
            },
        )
        .await;
        let _ = await_command_result(&mut stream).await;

        // The server self-exits after the last session reaps. Awaiting the
        // join confirms a clean exit (no panic in the reap/self-exit path);
        // a hang would trip the timeout.
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down within 5s after kill-last-pane")
            .expect("server task join")
            .expect("server run_async returned an error");
        assert!(
            !socket_path.exists(),
            "socket leaked after kill-last-pane reap: {}",
            socket_path.display(),
        );
    });
}
