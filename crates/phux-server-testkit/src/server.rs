//! Explicit server/Terminal composition shared by wire fixtures.

use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::policy::PolicyEngine;
use phux_server::{LaunchCommand, ServerConfig, ServerError, ServerRuntime};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, join_after_shutdown, recv_until, send_frame,
    wait_for_raw_socket, wait_for_socket,
};

pub type ServerHandles = (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>);

/// The one way to spawn an in-process server: [`SpawnedServer::builder`].
/// Every other spawn helper here is a wrapper over it.
///
/// With no options the server seeds nothing, runs no PTYs, and decides HELLO
/// with the posture `phux server` would pick. Options apply in call order,
/// [`Self::configure`] closures included, so a later one wins.
#[must_use = "a builder spawns nothing until `start`, `start_in`, or `spawn_at`"]
pub struct ServerBuilder<'a> {
    session: Option<String>,
    edits: Vec<ConfigEdit<'a>>,
}

/// One deferred [`ServerConfig`] edit.
type ConfigEdit<'a> = Box<dyn FnOnce(&mut ServerConfig) + 'a>;

impl<'a> ServerBuilder<'a> {
    /// Pre-seed session `name` (without a PTY unless [`Self::pty`] or
    /// [`Self::seed`] says otherwise).
    pub fn session(self, name: &str) -> Self {
        self.maybe_session(Some(name))
    }

    /// [`Self::session`] when `name` is `Some`.
    pub fn maybe_session(mut self, name: Option<&str>) -> Self {
        self.session = name.map(str::to_owned);
        self
    }

    /// Run panes (seed and attach-create) on real PTYs, with no override
    /// command, so a wire `CREATE_SESSION { command }` decides what runs.
    pub fn pty(self) -> Self {
        self.configure(|cfg| cfg.seed_with_pty = true)
    }

    /// Seed panes with a real PTY running `cmd`.
    pub fn seed(self, cmd: LaunchCommand) -> Self {
        self.configure(|cfg| seed_pty(cfg, cmd))
    }

    /// Decide every HELLO with `engine` (ADR-0072).
    pub fn policy(self, engine: Arc<dyn PolicyEngine>) -> Self {
        self.configure(|cfg| cfg.policy_engine = Some(engine))
    }

    /// Edit the [`ServerConfig`] directly.
    pub fn configure(mut self, edit: impl FnOnce(&mut ServerConfig) + 'a) -> Self {
        self.edits.push(Box::new(edit));
        self
    }

    /// Serve on `phux.sock` in a fresh temp dir.
    pub fn start(self) -> SpawnedServer {
        self.start_in(TempDir::new().unwrap())
    }

    /// Serve on `phux.sock` in `tmp`, which the caller may have populated.
    pub fn start_in(self, tmp: TempDir) -> SpawnedServer {
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = self.spawn_at(socket.clone());
        SpawnedServer {
            tmp,
            socket,
            shutdown,
            server,
        }
    }

    /// Serve on `socket_path`, which the caller owns (a short path, or one
    /// beside other fixtures). Returns the shutdown sender and join handle.
    ///
    /// Spawns on the current `LocalSet` (ADR-0014: pane actors are `!Send`,
    /// so call from inside [`crate::run_local`]).
    pub fn spawn_at(self, socket_path: PathBuf) -> ServerHandles {
        let mut cfg = ServerConfig {
            socket_path,
            pre_seeded_session: self.session,
            seed_with_pty: false,
            seed_command: None,
            ..ServerConfig::with_default_socket()
        };
        for edit in self.edits {
            edit(&mut cfg);
        }
        let (tx, rx) = oneshot::channel::<()>();
        let handle = tokio::task::spawn_local(async move {
            terminal_server(cfg)
                .run_async(async move {
                    let _ = rx.await;
                })
                .await
        });
        (tx, handle)
    }
}

/// Spawn a [`ServerRuntime`] on the current `LocalSet` (ADR-0014: pane actors
/// are `!Send`, so call from inside [`crate::run_local`]), optionally pre-seeding a
/// session without a PTY. Returns the shutdown sender and join handle.
pub fn spawn_server(socket_path: PathBuf, pre_seeded: Option<&str>) -> ServerHandles {
    SpawnedServer::builder()
        .maybe_session(pre_seeded)
        .spawn_at(socket_path)
}

/// Like [`spawn_server`] but lets the caller edit the [`ServerConfig`] first.
pub fn spawn_server_with<'a>(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
    configure: impl FnOnce(&mut ServerConfig) + 'a,
) -> ServerHandles {
    SpawnedServer::builder()
        .maybe_session(pre_seeded)
        .configure(configure)
        .spawn_at(socket_path)
}

/// Pre-seed `pre_seeded` with a PTY-backed pane running `cmd`.
pub fn spawn_server_with_seed_cmd(
    socket_path: PathBuf,
    pre_seeded: &str,
    cmd: LaunchCommand,
) -> ServerHandles {
    SpawnedServer::builder()
        .session(pre_seeded)
        .seed(cmd)
        .spawn_at(socket_path)
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
    SpawnedServer::builder()
        .maybe_session(pre_seeded)
        .pty()
        .spawn_at(socket_path)
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
    /// Compose a server option by option.
    pub fn builder<'a>() -> ServerBuilder<'a> {
        ServerBuilder {
            session: None,
            edits: Vec::new(),
        }
    }

    /// Start a server, optionally pre-seeding session `pre_seeded`.
    pub fn start<'a>(
        pre_seeded: Option<&str>,
        configure: impl FnOnce(&mut ServerConfig) + 'a,
    ) -> Self {
        Self::builder()
            .maybe_session(pre_seeded)
            .configure(configure)
            .start()
    }

    /// [`Self::start`] in `tmp`, which the caller may have populated.
    pub fn start_in<'a>(
        tmp: TempDir,
        pre_seeded: Option<&str>,
        configure: impl FnOnce(&mut ServerConfig) + 'a,
    ) -> Self {
        Self::builder()
            .maybe_session(pre_seeded)
            .configure(configure)
            .start_in(tmp)
    }

    /// A server whose panes (seed and attach-create) run on real PTYs.
    pub fn pty(pre_seeded: Option<&str>) -> Self {
        Self::builder().maybe_session(pre_seeded).pty().start()
    }

    /// Pre-seed `session` with a PTY-backed pane running `cmd`.
    pub fn seeded(session: &str, cmd: LaunchCommand) -> Self {
        Self::builder().session(session).seed(cmd).start()
    }

    /// [`Self::seeded`] in `tmp`, which the caller may have populated.
    pub fn seeded_in(tmp: TempDir, session: &str, cmd: LaunchCommand) -> Self {
        Self::builder().session(session).seed(cmd).start_in(tmp)
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
