//! Explicit server/Terminal composition shared by wire fixtures.

use phux_server::{LaunchCommand, ServerConfig, ServerError, ServerRuntime};
use std::path::PathBuf;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

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
