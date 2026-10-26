//! Sentences for server replies the client did not expect.
//!
//! The server's own `Error` message verbatim, or, for a reply kind this
//! client cannot interpret, a version-skew pointer to `phux doctor`; never a
//! `Debug` dump of a wire enum.

pub use phux_client_runtime::session::unexpected_reply;
use std::path::Path;

use phux_protocol::wire::frame::CommandResult;

/// Explain a [`CommandResult`] a caller did not expect, attributed to `verb`:
/// `"{verb} failed: {message}"` for an `Error`, else [`unexpected_reply`].
#[must_use]
pub fn explain_unexpected(verb: &str, result: &CommandResult) -> String {
    match result {
        CommandResult::Error { message, .. } => format!("{verb} failed: {message}"),
        _ => unexpected_reply(verb),
    }
}

/// Explain a request withheld because the server does not advertise the
/// `feature` bit it needs: the server is older than this client, so the
/// remedy is `phux upgrade`, which hands it this phux in place.
#[must_use]
pub fn server_too_old(what: &str, feature: &str) -> String {
    format!(
        "the server is too old for {what} (it does not advertise {feature}), so nothing was \
         sent; run `phux upgrade` to hand it this phux, then retry"
    )
}

/// Stop command when the server pid is not known. Bare `pkill phux` also
/// kills attached clients, so the pattern is the server process only.
const FALLBACK_STOP: &str = "pkill -f 'phux server'";

/// Lines for a live server that refused this binary's protocol.
///
/// `pid` is the server process when it is known. The fallback never suggests
/// `phux upgrade` and never calls the socket stale: stopping this process
/// closes the panes it is running.
#[must_use]
pub fn old_server_recovery_lines(socket: &Path, pid: Option<i32>) -> Vec<String> {
    let stop = pid.map_or_else(
        || format!("stop it with `{FALLBACK_STOP}`, then start a fresh one with `phux server`"),
        |pid| {
            format!(
                "stop it with `kill {pid}` (or `{FALLBACK_STOP}`), then start a fresh one with \
                 `phux server`"
            )
        },
    );
    vec![
        format!(
            "the server on {} is a live phux server from an older release and does not speak \
             this binary's protocol",
            socket.display()
        ),
        stop,
        "stopping it closes the panes it is running".to_owned(),
        format!("server log: {}", server_log_path().display()),
    ]
}

/// The server's pid: Unix-socket peer credentials, then the latest
/// `phux_server::health` record when that pid is still alive.
///
/// Peer credentials are an OS fact available on connect, before `HELLO`.
/// `Connection::connect` drops them when `HELLO` is refused, so this dials
/// the socket without a handshake.
#[must_use]
pub fn server_pid(socket: &Path) -> Option<i32> {
    peer_pid(socket).or_else(live_health_pid)
}

/// Pid from the socket's peer credentials, or `None` when nothing is listening
/// or the platform omits the pid.
///
/// The dial is a standard-library connect, so it does not wait on `HELLO`.
/// Credentials are read through tokio, which is the stable `SO_PEERCRED` /
/// `LOCAL_PEEREPID` path (`std`'s `peer_cred` is still unstable). That read
/// needs a reactor: use the one already running, or a private current-thread
/// runtime when the caller is synchronous.
#[must_use]
pub fn peer_pid(socket: &Path) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let stream = UnixStream::connect(socket).ok()?;
        stream.set_nonblocking(true).ok()?;
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio_peer_pid(stream)
        } else {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
                .ok()?;
            runtime.block_on(async move { tokio_peer_pid(stream) })
        }
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        None
    }
}

/// `SO_PEERCRED` / `LOCAL_PEEREPID` on a connected stream. The caller has a
/// tokio reactor entered.
#[cfg(unix)]
fn tokio_peer_pid(stream: std::os::unix::net::UnixStream) -> Option<i32> {
    tokio::net::UnixStream::from_std(stream)
        .ok()?
        .peer_cred()
        .ok()?
        .pid()
}

/// Whether `pid` names a process this caller can see.
#[must_use]
pub fn pid_is_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = rustix::process::Pid::from_raw(pid) else {
            return false;
        };
        // Signal 0 checks existence. `PERM` means it exists and is not ours.
        matches!(
            rustix::process::test_kill_process(pid),
            Ok(()) | Err(rustix::io::Errno::PERM)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// `<state>/server.log`, the same path `phux_server::telemetry::server_log_path`
/// reports.
fn server_log_path() -> std::path::PathBuf {
    phux_config::instance::state_dir().join("server.log")
}

/// The latest pid in `<state>/server-starts.log`, the record
/// `phux_server::health` appends (`<epoch-secs> <pid> <version>`).
fn live_health_pid() -> Option<i32> {
    let raw = health_record_pid(&std::fs::read_to_string(health_log_path()).ok()?)?;
    let pid = i32::try_from(raw).ok()?;
    pid_is_alive(pid).then_some(pid)
}

fn health_log_path() -> std::path::PathBuf {
    phux_config::instance::state_dir().join("server-starts.log")
}

/// The last parseable record's pid. A corrupt line is skipped, matching
/// `phux_server::health`.
fn health_record_pid(body: &str) -> Option<u32> {
    body.lines().rev().find_map(parse_health_pid)
}

fn parse_health_pid(line: &str) -> Option<u32> {
    let mut parts = line.splitn(3, ' ');
    let _at: u64 = parts.next()?.parse().ok()?;
    let pid = parts.next()?.parse().ok()?;
    parts.next()?;
    Some(pid)
}

#[cfg(test)]
mod tests {
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::wire::frame::{CommandValue, ErrorCode};

    use super::*;

    #[test]
    fn error_message_passes_through_verbatim() {
        let result = CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "pane @9 does not exist".to_owned(),
        };
        assert_eq!(
            explain_unexpected("kill", &result),
            "kill failed: pane @9 does not exist",
        );
    }

    #[test]
    fn server_too_old_names_the_feature_and_the_upgrade() {
        let sentence = server_too_old("kill --server", "SHUTDOWN");
        assert!(sentence.contains("too old for kill --server"), "{sentence}");
        assert!(sentence.contains("SHUTDOWN"), "{sentence}");
        assert!(sentence.contains("`phux upgrade`"), "{sentence}");
    }

    /// An unknown reply kind names `phux doctor` and the protocol triple,
    /// and never leaks a `Debug` render.
    #[test]
    fn unknown_kind_names_doctor_and_the_protocol_triple() {
        let triple = format!(
            "client protocol {}.{}.{}",
            PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor, PROTOCOL_VERSION.patch,
        );
        for sentence in [
            explain_unexpected(
                "GET_STATE",
                &CommandResult::OkWith(CommandValue::Bytes(vec![1, 2, 3])),
            ),
            explain_unexpected("GET_STATE", &CommandResult::Ok),
        ] {
            assert!(
                sentence.contains("unexpected GET_STATE reply"),
                "{sentence}"
            );
            assert!(sentence.contains("run `phux doctor`"), "{sentence}");
            assert!(sentence.contains(&triple), "{sentence}");
            assert!(!sentence.contains('{'), "{sentence}");
        }
    }

    #[test]
    fn old_server_recovery_names_the_pid_or_the_server_only_fallback() {
        let socket = Path::new("/tmp/phux.sock");
        let with_pid = old_server_recovery_lines(socket, Some(4242)).join("\n");
        assert!(with_pid.contains("`kill 4242`"), "{with_pid}");
        assert!(with_pid.contains("pkill -f 'phux server'"), "{with_pid}");
        assert!(with_pid.contains("`phux server`"), "{with_pid}");
        assert!(with_pid.contains("closes the panes"), "{with_pid}");
        assert!(with_pid.contains("/server.log"), "{with_pid}");
        assert_recovery_is_clean(&with_pid);

        let unknown = old_server_recovery_lines(socket, None).join("\n");
        assert!(unknown.contains("pkill -f 'phux server'"), "{unknown}");
        assert!(!unknown.contains("`kill "), "{unknown}");
        assert_recovery_is_clean(&unknown);
    }

    fn assert_recovery_is_clean(text: &str) {
        for forbidden in ["phux upgrade", "stale", "survive", "remove", "pkill phux"] {
            assert!(!text.contains(forbidden), "{forbidden} in {text}");
        }
    }

    #[test]
    fn health_record_pid_is_the_last_parseable_line() {
        let body = "not a record\n100 9 0.9.0\n200 nope 0.9.0\n300 4242 0.9.1\n";
        assert_eq!(health_record_pid(body), Some(4242));
        assert_eq!(parse_health_pid("300 4242 0.9.1"), Some(4242));
        assert_eq!(parse_health_pid("garbage"), None);
    }
}
