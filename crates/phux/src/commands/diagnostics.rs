//! The connect-failure diagnostics every verb prints: "no server" and
//! attach errors, each ending with its remedy.

use std::path::Path;
use std::process::ExitCode;

use phux_client::attach::{AttachError, is_incompatible_protocol};

use crate::commands::json_err;

/// Hint for a socket file with nothing listening.
pub(crate) const STALE_SOCKET_HINT: &str =
    "the socket may be stale — remove it and start a fresh server";

/// Hint when a socket answered, or failed, for a reason that is not a dead
/// socket and not an old protocol. Never calls the socket stale.
pub(crate) const UNANSWERED_SOCKET_HINT: &str = "see the server log for why this request failed";

/// How a socket probe failed, for `phux doctor`.
pub(crate) enum SocketFailure {
    /// A live server from an older release. `lines` is the recovery.
    Incompatible(Vec<String>),
    /// `ConnectionRefused` or `NotFound`: nothing is listening.
    Dead,
    /// Any other failure. Not a dead socket.
    Other,
}

/// Classify `err` for doctor. A live incompatible server is never [`SocketFailure::Dead`].
pub(crate) fn socket_failure(socket: &Path, err: &AttachError, pid: Option<i32>) -> SocketFailure {
    if is_incompatible_protocol(err) {
        SocketFailure::Incompatible(old_server_recovery_lines(socket, pid))
    } else if is_dead_socket(err) {
        SocketFailure::Dead
    } else {
        SocketFailure::Other
    }
}

/// The recovery for a live server that does not speak this binary's protocol.
///
/// Shared with MCP through `phux_client::explain::old_server_recovery_lines`.
pub(crate) fn old_server_recovery_lines(socket: &Path, pid: Option<i32>) -> Vec<String> {
    phux_client::explain::old_server_recovery_lines(socket, pid)
}

/// The server pid when `err` is an incompatible protocol, otherwise `None`.
pub(crate) fn recovery_pid(err: &AttachError, socket: &Path) -> Option<i32> {
    is_incompatible_protocol(err)
        .then(|| server_pid(socket))
        .flatten()
}

/// Peer credentials for this socket only; a live health-log pid may belong
/// to another profile or a different process after pid reuse.
pub(crate) fn server_pid(socket: &Path) -> Option<i32> {
    phux_client::explain::server_pid(socket)
}

fn is_dead_socket(err: &AttachError) -> bool {
    matches!(
        err,
        AttachError::Io(io) if matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
        )
    )
}

/// Print a "no server" diagnostic for a connect-time error, or a generic
/// one otherwise. Returns the failure exit code for the caller to bubble.
///
/// Every arm ends with its remedy (phux-i0e8.7.3): the no-server arm names
/// the exact start commands, and all arms name the canonical server log and
/// `phux doctor` — the two places the *reason* lives when the sentence here
/// is not enough.
pub(crate) fn report_no_server(err: &AttachError, socket_path: &Path, verb: &str) -> ExitCode {
    crate::output::error_block(
        no_server_lines(
            err,
            socket_path,
            verb,
            &phux_server::telemetry::server_log_path(),
            recovery_pid(err, socket_path),
        )
        .iter()
        .map(String::as_str),
    );
    ExitCode::FAILURE
}

/// The lines [`report_no_server`] prints, pure so tests can pin every arm
/// without capturing stderr (the `session_lines` pattern in `ls.rs`).
///
/// Continuation lines are indented two spaces so the remedy block reads as
/// one diagnostic, not four independent errors.
pub(super) fn no_server_lines(
    err: &AttachError,
    socket_path: &Path,
    verb: &str,
    server_log: &Path,
    pid: Option<i32>,
) -> Vec<String> {
    let mut lines = match err {
        AttachError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
            ) =>
        {
            vec![
                format!("no server running at {}", socket_path.display()),
                "start one with `phux` (attaches, auto-starting a server) or `phux server --ensure`"
                    .to_owned(),
            ]
        }
        AttachError::Disconnected => {
            vec![format!("server closed the connection during {verb}")]
        }
        AttachError::Io(io_err) => vec![json_err::unreachable_socket_message(
            verb,
            io_err,
            socket_path,
        )],
        AttachError::IncompatibleProtocol(_) => {
            return old_server_recovery_lines(socket_path, pid);
        }
        other => vec![format!("{verb} failed: {other}")],
    };
    lines.push(format!("server log: {}", server_log.display()));
    lines.push("run `phux doctor` for a health check".to_owned());
    lines
}

/// Print an `AttachError` as a one-line, actionable message on stderr.
///
/// `phux-roz` (5): the previous output was `attach failed: connection
/// refused` — accurate but useless. The new shape names the socket and
/// `phux server --ensure`, so the user can copy-paste their way out of
/// the failure mode. A bare `phux server` stays in the foreground and
/// dies with the terminal. `--ensure` conflicts with `--session`.
pub(crate) fn print_attach_error(err: &AttachError, socket_path: &Path, session: &str) {
    crate::output::error_block(
        attach_error_lines(
            err,
            socket_path,
            session,
            &phux_server::telemetry::default_client_log_path(),
            recovery_pid(err, socket_path),
        )
        .iter()
        .map(String::as_str),
    );
}

/// The lines [`print_attach_error`] prints, pure so tests can pin every arm.
///
/// The first three arms are self-explaining (each names its own remedy or
/// cause), so they stay single-line. `Disconnected` gets its own arm
/// (phux-i0e8.2.3): the server vanished mid-session, so the remedy is its
/// log and doctor, not this client's. The fallthrough —
/// `Protocol`/`Terminal`/`Ghostty`/… — is where the sentence alone was a
/// dead end (phux-i0e8.7.3): those failures leave their reason in this
/// client's own log, and a `Protocol` error in particular usually means the
/// binaries disagree, so the remedy block names the client log, `phux
/// doctor`, and this client's protocol triple for the comparison.
pub(super) fn attach_error_lines(
    err: &AttachError,
    socket_path: &Path,
    _session: &str,
    client_log: &Path,
    pid: Option<i32>,
) -> Vec<String> {
    match err {
        AttachError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
            ) =>
        {
            vec![format!(
                "no server at {}. Start one with: phux server --ensure",
                socket_path.display()
            )]
        }
        AttachError::Refused(message) => {
            vec![format!("server refused attach: {message}")]
        }
        AttachError::Io(io_err) => vec![
            json_err::unreachable_socket_message("attach", io_err, socket_path),
            "run `phux doctor` for a health check".to_owned(),
        ],
        AttachError::NotATty => {
            vec!["attach requires an interactive terminal (stdin is not a TTY).".to_owned()]
        }
        // A mid-session disconnect outside the reconnect window (e.g. `phux
        // new`'s attach tail): the reason lives in the server log.
        AttachError::Disconnected => vec![
            "the server closed the connection unexpectedly".to_owned(),
            format!(
                "server log: {}",
                phux_server::telemetry::server_log_path().display()
            ),
            "run `phux doctor` for a health check".to_owned(),
        ],
        AttachError::IncompatibleProtocol(_) => old_server_recovery_lines(socket_path, pid),
        other => {
            let version = phux_protocol::PROTOCOL_VERSION;
            vec![
                format!("attach failed: {other}"),
                format!("client log: {}", client_log.display()),
                format!(
                    "run `phux doctor` for a health check (client protocol {}.{}.{})",
                    version.major, version.minor, version.patch,
                ),
            ]
        }
    }
}
