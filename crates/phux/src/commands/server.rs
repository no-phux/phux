use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_config::socket::default_socket_path;

pub(super) mod ensure;
mod permissions;
mod run;
mod spawn;
mod startup;

pub use ensure::ENSURE_TIMEOUT_ENV;
pub(crate) use permissions::{PrivacyService, run as run_permissions};
pub(crate) use run::run_server;
pub use spawn::AUTO_SPAWN_IDLE_ENV;
pub(crate) use spawn::{ensure_server, ensure_server_for_bootstrap, ensure_server_unseeded};

/// How long the auto-spawn path waits for a fresh server to accept. Bind is
/// sub-ms on a healthy system, but startup first waits up to 3s for the
/// state directory's audit lock while another server shares it; a shorter
/// deadline gave up on a server about to accept and spawned a duplicate.
/// A server that exits early is noticed at once, so failures stay fast.
const AUTO_SPAWN_SOCKET_TIMEOUT: Duration = Duration::from_secs(6);

/// Poll cadence while waiting for the auto-spawned server's socket.
const AUTO_SPAWN_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a client waits for the spawn lock before spawning unserialised:
/// long enough to observe the holder's spawn, short enough not to hang.
const SPAWN_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Version of the `phux server --ensure --json` availability document.
pub(crate) const ENSURE_SCHEMA_VERSION: u8 = 1;

/// Answer a running server that is about to re-exec into this binary: could
/// this image resume from the blob on stdin? The server refuses the upgrade
/// on a failure here, while every pane is still alive.
pub(crate) fn answer_upgrade_preflight() -> ExitCode {
    let mut blob = Vec::new();
    let verdict = std::io::Read::read_to_end(&mut std::io::stdin(), &mut blob)
        .map_err(|error| format!("read upgrade blob: {error}"))
        .and_then(|_| phux_server::upgrade::blob::preflight(&blob));
    match verdict {
        Ok(()) => {
            let passed = phux_server::upgrade::blob::PREFLIGHT_PASSED;
            crate::output::bytes(format!("{passed}\n").as_bytes());
            ExitCode::SUCCESS
        }
        Err(reason) => {
            crate::output::error_block(reason.lines());
            ExitCode::FAILURE
        }
    }
}

/// Which owner made the selected socket available. `--ensure` proves only
/// that the socket accepts; HELLO still negotiates protocol and features.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnsureDisposition {
    /// The socket accepted before startup coordination was needed.
    Reused,
    /// Another caller made it available while this caller waited for the lock.
    Joined,
    /// A pending `service install --adopt` handover started the supervisor.
    SupervisedStarted,
    /// This process launched the detached server directly.
    DaemonStarted,
}

impl EnsureDisposition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reused => "reused",
            Self::Joined => "joined",
            Self::SupervisedStarted => "supervised_started",
            Self::DaemonStarted => "daemon_started",
        }
    }
}

fn ensure_document(socket_path: &Path, disposition: EnsureDisposition) -> serde_json::Value {
    serde_json::json!({
        "schema_version": ENSURE_SCHEMA_VERSION,
        "running": true,
        "socket": socket_path.display().to_string(),
        "disposition": disposition.as_str(),
        "cli_version": env!("CARGO_PKG_VERSION"),
        "server_log": phux_config::instance::server_log_path(),
    })
}

fn report_ensure_failure(json: bool, socket_path: &Path, err: &std::io::Error) -> ExitCode {
    if !json {
        error!("server --ensure: {}: {err}", socket_path.display());
        return ExitCode::FAILURE;
    }

    let code = match err.kind() {
        std::io::ErrorKind::TimedOut => super::json_err::codes::SERVER_START_TIMEOUT,
        std::io::ErrorKind::Interrupted => super::json_err::codes::SERVER_START_CANCELLED,
        _ => super::json_err::codes::SERVER_START_FAILED,
    };
    let log_path = phux_config::instance::server_log_path();
    let error = super::json_err::CliError::new(
        code,
        format!(
            "could not make the local server available at {}: {err}",
            socket_path.display()
        ),
        format!(
            "retry; inspect {}; run `phux config check` and `phux doctor`",
            log_path.display()
        ),
    );
    super::json_err::emit(true, &error, 1)
}

/// Noninteractive counterpart of naked `phux`'s coordinator startup. A
/// server it starts seeds naked `phux`'s session unless `seed` is false
/// (`--no-seed`): an app that picks its own session wants none made for it.
pub(crate) fn run_ensure(socket: Option<PathBuf>, json: bool, seed: bool) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    if let Err(err) = phux_server::runtime::validate_socket_path_len(&socket_path) {
        return report_ensure_failure(
            json,
            &socket_path,
            &std::io::Error::new(std::io::ErrorKind::InvalidInput, err),
        );
    }
    match ensure::with_deadline(socket_path.clone(), seed) {
        Ok(disposition) => {
            if json {
                outln!("{}", ensure_document(&socket_path, disposition));
            }
            ExitCode::SUCCESS
        }
        Err(err) => report_ensure_failure(json, &socket_path, &err),
    }
}

fn ensure_accepting(socket_path: &Path, seed: bool) -> std::io::Result<EnsureDisposition> {
    // Availability-only: the quiet path skips version reconciliation, so a
    // bundled CLI does not re-exec another installation's coordinator.
    let disposition = if seed {
        ensure_server(
            socket_path,
            &super::attach::resolved_default_session_name(),
            super::attach::configured_spawn_on_attach().as_deref(),
            true,
        )?
    } else {
        ensure_server_unseeded(socket_path, true)?
    };
    // The probe calls permission errors "Live" (to avoid unlinking another
    // user's socket); success here requires a real connect.
    std::os::unix::net::UnixStream::connect(socket_path).map(|stream| {
        drop(stream);
        disposition
    })
}

/// Run `phux server` as parsed. `--ensure` returned before tracing, in
/// `crate::run`; `server permissions` needs no server.
pub(crate) fn run(args: crate::commands::ServerArgs, socket: Option<PathBuf>) -> ExitCode {
    if let Some(crate::commands::ServerAction::Permissions { json, open }) = args.action {
        return run_permissions(socket, json.json, open);
    }
    let crate::commands::ServerArgs {
        action: _,
        // --ensure returns from run before tracing and this dispatch.
        ensure: _,
        json: _,
        session,
        listen,
        quic,
        webtransport,
        connect,
        hub,
        exit_after_idle,
        autosave,
        daemonize,
        seed_command,
        resume,
        no_seed,
    } = args;
    // An upgrade re-execs with the old argv; a supervised server takes the
    // listeners its unit names now (`service install --adopt` rewrote it).
    let (listen, quic) = match (resume, &socket) {
        (Some(_), Some(path)) => crate::commands::service::listeners_on_resume(path, listen, quic),
        _ => (listen, quic),
    };
    run_server(
        (!no_seed).then_some(session.as_str()),
        socket,
        listen,
        quic,
        webtransport,
        connect,
        hub,
        exit_after_idle,
        autosave.as_deref(),
        daemonize,
        seed_command.as_deref(),
        resume,
    )
}

#[cfg(test)]
mod tests;
