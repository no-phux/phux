//! The server process's own lifecycle: the UDS listener (stale socket, busy
//! path, partial frames, unlink on shutdown), self-exit once the last
//! session goes (only after serving a client, never while a keep-empty
//! session parks), and the opt-in `exit_after_idle` lifetime (ADR-0063).
//! Last-shell natural exit replacing the shell is `attach/eof_detach.rs`.

use std::time::Duration;

use phux_protocol::wire::frame::{
    Command, FrameKind, SESSION_KEEP_EMPTY_KEY, Scope, encode_session_keep_empty,
};
use phux_server::ServerError;
use phux_server::runtime::{ServerConfig, ServerRuntime};
use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, join_after_shutdown, recv_typed, run_local,
    send_frame, spawn_server, wait_for_raw_socket, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::common::{attach, session, state, wait_for_state};

/// A server with a PTY seed pane running `sh -c script` in session `solo`,
/// whose run future never resolves: only the server itself can end it.
fn seeded(
    script: &str,
    exit_after_idle: Option<Duration>,
) -> (
    TempDir,
    std::path::PathBuf,
    JoinHandle<Result<(), ServerError>>,
) {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("phux.sock");
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", script]);
    let cfg = ServerConfig {
        socket_path: socket_path.clone(),
        pre_seeded_session: Some("solo".to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        exit_after_idle,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(cfg)
            .run_async(std::future::pending::<()>())
            .await
    });
    (tmp, socket_path, handle)
}

async fn assert_exits(handle: JoinHandle<Result<(), ServerError>>, why: &str) {
    timeout(SERVER_JOIN_DEADLINE, handle)
        .await
        .unwrap_or_else(|_| panic!("server did not exit: {why}"))
        .expect("server task join")
        .expect("a clean self-exit");
}

async fn assert_stays_up(handle: JoinHandle<Result<(), ServerError>>, window: Duration, why: &str) {
    assert!(
        timeout(window, handle).await.is_err(),
        "server exited: {why}"
    );
}

async fn ping(stream: &mut tokio::net::UnixStream, nonce: u64) {
    send_frame(stream, &FrameKind::Ping { nonce }).await;
    assert_eq!(recv_typed(stream).await.1, FrameKind::Pong { nonce });
}

/// A stale file at the socket path is replaced; a client that sends half a
/// length prefix and vanishes does not hurt the server; PING answers; and
/// clean shutdown unlinks the socket.
#[test]
fn listener_recovers_a_stale_path_survives_partial_frames_and_unlinks() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        std::fs::write(&socket_path, b"stale leftover").unwrap();
        let (shutdown, handle) = spawn_server(socket_path.clone(), None);

        let mut partial = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        partial.write_all(&[0x00, 0x09]).await.unwrap();
        drop(partial);
        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        ping(&mut stream, 0xCAFE_BABE_1234_5678).await;

        drop(stream);
        join_after_shutdown(shutdown, handle).await;
        assert!(!socket_path.exists(), "the socket is unlinked on shutdown");
    });
}

#[test]
fn a_second_server_on_a_live_socket_is_socket_busy() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown, handle) = spawn_server(socket_path.clone(), None);
        let _probe = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let cfg = ServerConfig {
            socket_path: socket_path.clone(),
            ..ServerConfig::with_default_socket()
        };
        let second = ServerRuntime::new(cfg)
            .run_async(std::future::pending::<()>())
            .await;
        assert!(
            matches!(second, Err(ServerError::SocketBusy(ref p)) if *p == socket_path),
            "{second:?}"
        );
        join_after_shutdown(shutdown, handle).await;
    });
}

#[test]
fn killing_the_last_pane_self_exits_after_serving_a_client() {
    run_local(async {
        let (_tmp, socket, handle) = seeded("read _", None);
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let pane = attach(&mut stream, "solo").await.focused_resource;
        let kill = Command::KillResources {
            ids: vec![pane],
            operation_id: None,
        };
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 2,
                command: kill,
            },
        )
        .await;
        assert_exits(handle, "its only pane was killed").await;
    });
}

/// A server that never served a client must survive its seed pane dying,
/// or `phux`'s auto-spawn races the exit and the user sees "no server".
#[test]
fn a_server_without_clients_survives_its_seed_pane_dying() {
    run_local(async {
        let (_tmp, socket, handle) = seeded("exit 0", None);
        let _stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        assert_stays_up(handle, Duration::from_secs(1), "before serving any client").await;
    });
}

/// Close Tab of a keep-empty session's last pane leaves it empty, and that
/// parked session holds the server up with zero processes.
#[test]
fn a_parked_keep_empty_session_holds_the_server_up() {
    run_local(async {
        let (_tmp, socket, handle) = seeded("read _", None);
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let pane = attach(&mut stream, "solo").await.focused_resource;
        let mark = FrameKind::SetMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: SESSION_KEEP_EMPTY_KEY.to_owned(),
            value: encode_session_keep_empty("solo", true),
        };
        send_frame(&mut stream, &mark).await;
        // The mark has no reply: confirm it before closing the last pane.
        assert!(
            session(&state(&mut stream, 2).await, "solo")
                .unwrap()
                .keep_empty
        );
        let close = Command::CloseTabResources { ids: vec![pane] };
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 3,
                command: close,
            },
        )
        .await;
        wait_for_state(&mut stream, 10, "the last pane to go", |s| {
            session(s, "solo").is_some_and(|solo| solo.window_count == 0 && solo.keep_empty)
        })
        .await;
        assert_stays_up(
            handle,
            Duration::from_secs(1),
            "a keep-empty session remains",
        )
        .await;
    });
}

const IDLE_LIMIT: Duration = Duration::from_millis(200);

/// With `exit_after_idle`, an open connection (even one that never
/// attaches, like `phux send-keys`) disarms the clock; once the last
/// connection closes the server exits despite its live pane.
#[test]
fn idle_lifetime_waits_for_the_last_connection_then_exits() {
    run_local(async {
        let (_tmp, socket, handle) = seeded("sleep 600", Some(IDLE_LIMIT));
        let stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        tokio::time::sleep(IDLE_LIMIT * 4).await;
        assert!(!handle.is_finished(), "exited while a connection was open");
        drop(stream);
        assert_exits(handle, "unattended past exit_after_idle").await;
    });
}

/// Without the lifetime an unattended server with a live pane stays up: the
/// idle exit is opt-in and must never become the default.
#[test]
fn without_an_idle_lifetime_an_unattended_server_stays_up() {
    run_local(async {
        let (_tmp, socket, handle) = seeded("sleep 600", None);
        drop(wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await);
        assert_stays_up(handle, IDLE_LIMIT * 4, "no exit_after_idle configured").await;
    });
}
