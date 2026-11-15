//! Explicit server/Terminal composition shared by wire fixtures.

use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::{LaunchCommand, ServerConfig, ServerError, ServerRuntime};
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, join_after_shutdown, recv_until, send_frame,
    wait_for_raw_socket, wait_for_socket,
};

pub type ServerHandles = (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>);

/// Spawn a [`ServerRuntime`] on the current `LocalSet` (ADR-0014: pane actors
/// are `!Send`, so call from inside [`crate::run_local`]), optionally pre-seeding a
/// session without a PTY. Returns the shutdown sender and join handle.
pub fn spawn_server(socket_path: PathBuf, pre_seeded: Option<&str>) -> ServerHandles {
    spawn_server_with(socket_path, pre_seeded, |_| {})
}

/// Like [`spawn_server`] but lets the caller edit the [`ServerConfig`] first.
pub fn spawn_server_with(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
    configure: impl FnOnce(&mut ServerConfig),
) -> ServerHandles {
    let (tx, rx) = oneshot::channel::<()>();
    let mut cfg = ServerConfig {
        socket_path,
        pre_seeded_session: pre_seeded.map(str::to_owned),
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    configure(&mut cfg);
    let handle = tokio::task::spawn_local(async move {
        terminal_server(cfg)
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Pre-seed `pre_seeded` with a PTY-backed pane running `cmd`.
pub fn spawn_server_with_seed_cmd(
    socket_path: PathBuf,
    pre_seeded: &str,
    cmd: LaunchCommand,
) -> ServerHandles {
    spawn_server_with(socket_path, Some(pre_seeded), |cfg| seed_pty(cfg, cmd))
}

/// Explicit test composition with the production Terminal implementation.
pub fn terminal_server(cfg: ServerConfig) -> ServerRuntime {
    ServerRuntime::new(cfg, std::sync::Arc::new(phux_terminal::TerminalEngine))
}

/// Configure `cfg` to seed panes with a real PTY running `cmd`.
pub fn seed_pty(cfg: &mut ServerConfig, cmd: LaunchCommand) {
    cfg.seed_with_pty = true;
    cfg.seed_command = Some(cmd);
}

/// Seed panes with a real PTY but no override command, so a wire
/// `CREATE_SESSION { command }` decides what runs.
pub fn spawn_server_seed_pty_no_cmd(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
) -> ServerHandles {
    spawn_server_with(socket_path, pre_seeded, |cfg| cfg.seed_with_pty = true)
}

/// A `/bin/sh -c` body that ignores `SIGHUP`, so only a pane kill's
/// `SIGKILL` backstop ends it. It also exits within a second of this test
/// process ending, so a run that panics, times out, or drops its server
/// mid-grace cannot orphan it, and with it a PTY.
pub fn hangup_proof_script() -> String {
    format!(
        "trap '' HUP; while kill -0 {} 2>/dev/null; do sleep 1; done",
        std::process::id()
    )
}

/// A server on `phux.sock` in its own temp dir; dropping it stops the server.
#[must_use = "dropping the shutdown sender stops the server"]
pub struct SpawnedServer {
    pub tmp: TempDir,
    pub socket: PathBuf,
    shutdown: oneshot::Sender<()>,
    server: JoinHandle<Result<(), ServerError>>,
}

impl SpawnedServer {
    /// Start a server, optionally pre-seeding session `pre_seeded`.
    pub fn start(pre_seeded: Option<&str>, configure: impl FnOnce(&mut ServerConfig)) -> Self {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_with(socket.clone(), pre_seeded, configure);
        Self {
            tmp,
            socket,
            shutdown,
            server,
        }
    }

    /// A server whose panes (seed and attach-create) run on real PTYs.
    pub fn pty(pre_seeded: Option<&str>) -> Self {
        Self::start(pre_seeded, |cfg| cfg.seed_with_pty = true)
    }

    /// Pre-seed `session` with a PTY-backed pane running `cmd`.
    pub fn seeded(session: &str, cmd: LaunchCommand) -> Self {
        Self::start(Some(session), |cfg| seed_pty(cfg, cmd))
    }

    /// Open another HELLO'd client against this server.
    pub async fn connect(&self) -> UnixStream {
        wait_for_socket(&self.socket, SOCKET_CONNECT_DEADLINE).await
    }

    /// Open a client that has sent nothing.
    pub async fn connect_raw(&self) -> UnixStream {
        wait_for_raw_socket(&self.socket, SOCKET_CONNECT_DEADLINE).await
    }

    /// A HELLO'd client already attached to session `name`, and its snapshot.
    pub async fn attach(&self, name: &str) -> (UnixStream, SessionSnapshot) {
        let mut stream = self.connect().await;
        send_frame(&mut stream, &attach_by_name(name)).await;
        let snapshot = recv_until(&mut stream, |_, frame| match frame {
            FrameKind::Attached { snapshot, .. } => Some(snapshot),
            _ => None,
        })
        .await;
        (stream, snapshot)
    }

    /// Shut down, assert a clean join, and assert the socket was unlinked.
    /// Drop client streams first.
    pub async fn stop(self) {
        join_after_shutdown(self.shutdown, self.server).await;
        assert!(!self.socket.exists(), "socket leaked after shutdown");
    }
}

/// Spawn a [`phux_server::ServerRuntime`] (optionally pre-seeded) in a temp dir and return
/// a HELLO'd client. Extra clients use [`SpawnedServer::connect`].
#[must_use = "dropping the shutdown sender stops the server"]
pub async fn spawn_server_connected(pre_seeded: Option<&str>) -> (SpawnedServer, UnixStream) {
    let server = SpawnedServer::start(pre_seeded, |_| {});
    let stream = server.connect().await;
    (server, stream)
}
