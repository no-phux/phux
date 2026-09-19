//! Lifecycle integration tests for last-shell replacement and server exit.
//!
//! Natural `exit` of a session's last shell respawns a default shell in the
//! same Terminal, so attached clients keep a live pane. Explicit
//! `KILL_RESOURCES` still reaps, and once the last session is gone the
//! server self-exits — **but only after it has served at least one
//! client**. A freshly auto-spawned server whose seed pane dies before
//! anyone attaches must stay alive so the launching `phux` can still
//! connect; otherwise the auto-spawn → attach flow races the server's
//! own self-exit and the user sees "no server".

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::time::Duration;

use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, ResourceLifecycle, StateScope, TYPE_ATTACHED,
};
use phux_server::runtime::{ServerConfig, ServerRuntime};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_command_result, recv_typed, run_local,
    send_frame, wait_for_socket,
};

/// Build a PTY-seeded server config whose seed pane runs `sh -c <script>`.
fn seeded_cfg(socket_path: std::path::PathBuf, script: &str) -> ServerConfig {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(script);
    ServerConfig {
        socket_path,
        pre_seeded_session: Some("solo".to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        ..ServerConfig::with_default_socket()
    }
}

/// Natural exit of the last shell keeps the session's Terminal live.
#[test]
fn last_natural_exit_keeps_a_fresh_shell() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let exited = tmp.path().join("exited");
        let cfg = seeded_cfg(
            socket_path.clone(),
            &format!(
                "until [ -f '{}' ]; do sleep 0.01; done; echo done > '{}'; exit 0",
                release.display(),
                exited.display()
            ),
        );
        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .run_async(std::future::pending::<()>())
                .await
        });

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("solo")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "attach must land before the pane exits",
        );

        std::fs::write(&release, b"go").expect("release the seed pane");
        let wait_exit = tokio::time::Instant::now() + phux_server_testkit::SERVER_JOIN_DEADLINE;
        while !exited.exists() {
            assert!(
                tokio::time::Instant::now() < wait_exit,
                "seed pane never recorded its exit"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let deadline = tokio::time::Instant::now() + phux_server_testkit::SERVER_JOIN_DEADLINE;
        let mut request_id = 10;
        loop {
            send_frame(
                &mut stream,
                &FrameKind::Command {
                    request_id,
                    command: Command::GetState {
                        scope: StateScope::Server,
                    },
                },
            )
            .await;
            let result = recv_command_result(&mut stream, request_id).await;
            let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
                panic!("GET_STATE failed: {result:?}");
            };
            let solo = snapshot
                .sessions
                .iter()
                .find(|s| s.name == "solo")
                .expect("the session must survive a natural last-shell exit");
            let live = snapshot.resources.iter().any(|resource| {
                resource.lifecycle == ResourceLifecycle::Running && resource.exit.is_none()
            });
            if solo.window_count == 1 && live {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "last-shell replacement never produced a live Terminal"
            );
            request_id += 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let still_running = timeout(Duration::from_secs(1), handle).await.is_err();
        assert!(
            still_running,
            "a replaced last shell must hold the server up",
        );
    });
}

/// Explicit kill of the last pane still reaps the session and self-exits.
#[test]
fn kill_last_pane_self_exits_after_serving_a_client() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cfg = seeded_cfg(socket_path.clone(), "read _");
        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .run_async(std::future::pending::<()>())
                .await
        });

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("solo")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("ATTACH must be answered with ATTACHED, got {attached:?}");
        };
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 2,
                command: Command::KillResources {
                    ids: vec![snapshot.focused_resource],
                    operation_id: None,
                },
            },
        )
        .await;

        let run = timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, handle)
            .await
            .expect("server did not self-exit within 5s after its only pane was killed")
            .expect("server task join");
        run.expect("run_async returned an error rather than a clean self-exit");
    });
}

/// ADR-0105: Close Tab of the last pane of a keep-empty session leaves it
/// empty. Natural `exit` would replace the shell instead.
#[test]
fn close_tab_of_keep_empty_last_pane_leaves_the_session_empty() {
    use phux_protocol::wire::frame::{SESSION_KEEP_EMPTY_KEY, Scope, encode_session_keep_empty};

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cfg = seeded_cfg(socket_path.clone(), "read _");
        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .run_async(std::future::pending::<()>())
                .await
        });

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("solo")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let FrameKind::Attached { snapshot, .. } = attached else {
            panic!("ATTACH must be answered with ATTACHED, got {attached:?}");
        };

        send_frame(
            &mut stream,
            &FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: SESSION_KEEP_EMPTY_KEY.to_owned(),
                value: encode_session_keep_empty("solo", true),
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 2,
                command: Command::CloseTabResources {
                    ids: vec![snapshot.focused_resource],
                },
            },
        )
        .await;

        let deadline = tokio::time::Instant::now() + phux_server_testkit::SERVER_JOIN_DEADLINE;
        let mut request_id = 10;
        loop {
            send_frame(
                &mut stream,
                &FrameKind::Command {
                    request_id,
                    command: Command::GetState {
                        scope: StateScope::Server,
                    },
                },
            )
            .await;
            let result = recv_command_result(&mut stream, request_id).await;
            let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
                panic!("GET_STATE failed: {result:?}");
            };
            let solo = snapshot
                .sessions
                .iter()
                .find(|s| s.name == "solo")
                .expect("the keep-empty session must survive Close Tab of its last pane");
            if solo.window_count == 0 {
                assert!(solo.keep_empty);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the seed pane was never reaped"
            );
            request_id += 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let still_running = timeout(Duration::from_secs(1), handle).await.is_err();
        assert!(
            still_running,
            "a keep-empty session must hold the server up with zero processes",
        );
    });
}

/// Auto-spawn grace: a server that has NEVER served a client must NOT
/// self-exit when its seed pane dies immediately — otherwise `phux`'s
/// auto-spawn races the server's exit and the user sees "no server".
#[test]
fn server_without_clients_does_not_self_exit_on_seed_pane_death() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // Dies immediately; no client ever attaches.
        let cfg = seeded_cfg(socket_path.clone(), "exit 0");

        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .run_async(std::future::pending::<()>())
                .await
        });

        // Confirm the server actually bound (so the "still running" assert
        // below is meaningful, not just "never started").
        let _stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Give the seed pane its death + the (suppressed) reap a full
        // window. The handle must still be pending — the server stayed up.
        let still_running = timeout(Duration::from_secs(1), handle).await.is_err();
        assert!(
            still_running,
            "server must NOT self-exit before serving any client (auto-spawn grace)",
        );
    });
}
