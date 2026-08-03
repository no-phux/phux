//! Lifecycle integration tests for the tmux server-exit model (phux-60s)
//! and its auto-spawn grace (phux-k61 follow-up).
//!
//! Contract: when a pane's process exits, the runtime reaps the pane,
//! cascading to its window and session. Once the last session is gone the
//! server self-exits — **but only after it has served at least one
//! client**. A freshly auto-spawned server whose seed pane dies before
//! anyone attaches must stay alive (empty) so the launching `phux` can
//! still connect and repopulate it; otherwise the auto-spawn → attach
//! flow races the server's own self-exit and the user sees "no server".

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::time::Duration;

use phux_protocol::wire::frame::TYPE_ATTACHED;
use phux_server::runtime::{ServerConfig, ServerRuntime};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame, wait_for_socket,
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

/// Served-then-reaped: a client attaches, the pane later exits, and the
/// server self-exits on its own (no Ctrl-C, no shutdown frame).
#[test]
fn server_self_exits_after_serving_a_client() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // Live long enough for the client to attach, then exit → reap.
        let cfg = seeded_cfg(socket_path.clone(), "sleep 0.3; exit 0");

        // `pending()` shutdown: the ONLY way `run_async` can return is the
        // reap-driven self-exit (armed once a client has attached).
        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .run_async(std::future::pending::<()>())
                .await
        });

        // Attach so the server has "served" a client.
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("solo")).await;
        let (type_byte, _attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "attach must land before the pane exits",
        );

        // The pane exits ~0.3s in; the reap then self-exits the server.
        let run = timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, handle)
            .await
            .expect("server did not self-exit within 5s after its only pane died")
            .expect("server task join");
        run.expect("run_async returned an error rather than a clean self-exit");
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
