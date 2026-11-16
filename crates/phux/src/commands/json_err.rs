//! The stable JSON error contract shared by every `--json` verb (ADR-0065
//! §4): a failure is one JSON line on stderr, stdout stays empty, and exit
//! codes are unchanged (`0` success, `1` miss / no server, `2` refusal / usage,
//! `3` partial view, `124`/`125` timeouts):
//!
//! ```json
//! {"schema_version":1,"error":{"code":"no_server","message":"..."},"remedy":"...","exit_code":1}
//! ```
//!
//! A `no_server` (or `transport`) failure after this process tried to
//! auto-start the server also carries the additive `error.auto_start_error`:
//! why that start failed, the server's own bind or config error included.
//!
//! Without `--json` the same failure prints the shared human error label and
//! indented hints; message values and machine documents remain unchanged.
//! The document, its [`CliError`], and the closed `code` vocabulary in
//! [`codes`] live in [`phux_client::contract`], shared with MCP; consumers
//! read `docs/scripting.md#json-output-and-errors`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use phux_client::attach::AttachError;

pub(crate) use phux_client::contract::{ERROR_SCHEMA_VERSION, codes, error_document};

/// One CLI failure: the shared [`phux_client::contract::Refusal`], under the
/// name the CLI's verbs already use.
pub(crate) type CliError = phux_client::contract::Refusal;

/// A verb's failure decided in one place: the error document and the status
/// to exit with.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) err: CliError,
    pub(crate) exit: u8,
}

impl Refusal {
    pub(crate) fn new(
        code: &'static str,
        message: impl Into<String>,
        remedy: impl Into<String>,
        exit: u8,
    ) -> Self {
        Self {
            err: CliError::new(code, message, remedy),
            exit,
        }
    }

    /// Report on the verb's own channels and return the exit code.
    pub(crate) fn emit(&self, json: bool) -> ExitCode {
        emit(json, &self.err, self.exit)
    }
}

/// The last auto-start failure this process saw, and the socket it was for.
/// A CLI process makes one auto-start attempt and then dials, and the dial's
/// error is reported far from the attempt, so the failure is parked here.
static AUTO_START_FAILURE: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

/// Remember that auto-starting a server on `socket_path` failed with
/// `error`, so a later [`no_server_error`] for that socket can say why.
pub(crate) fn record_auto_start_failure(socket_path: &Path, error: &std::io::Error) {
    if let Ok(mut slot) = AUTO_START_FAILURE.lock() {
        *slot = Some((socket_path.to_path_buf(), error.to_string()));
    }
}

/// The recorded auto-start failure for `socket_path`, if any.
fn auto_start_failure(socket_path: &Path) -> Option<String> {
    let slot = AUTO_START_FAILURE.lock().ok()?;
    slot.as_ref()
        .filter(|(socket, _)| socket == socket_path)
        .map(|(_, error)| error.clone())
}

/// Report `err` on stderr (one JSON line under `json`, prose plus indented
/// remedy otherwise) and return the exit code. stdout is never touched.
pub(crate) fn emit(json: bool, err: &CliError, exit_code: u8) -> ExitCode {
    if json {
        match serde_json::to_string(&error_document(err, exit_code)) {
            // One unstyled JSON document is the stderr protocol.
            Ok(line) => eprintln!("{line}"),
            // A Value of strings and numbers cannot fail to serialize; the
            // fallback keeps the failure visible rather than silent.
            Err(_) => eprintln!(
                "{{\"schema_version\":1,\"error\":{{\"code\":\"json_serialize\",\"message\":\"could not render JSON error\"}},\"remedy\":\"run phux doctor and report this bug\",\"exit_code\":{exit_code}}}"
            ),
        }
    } else {
        error!("{}", err.message);
        if !err.remedy.is_empty() {
            for line in err.remedy.lines() {
                hint!("{line}");
            }
        }
    }
    ExitCode::from(exit_code)
}

/// Json-aware [`crate::commands::report_no_server`]: shared human diagnostics
/// otherwise; under `json`, `no_server` for a connect failure or
/// `server_disconnected` / `transport` otherwise, always exit 1.
pub(crate) fn report_no_server(
    json: bool,
    err: &AttachError,
    socket_path: &Path,
    verb: &str,
) -> ExitCode {
    if !json {
        return crate::commands::report_no_server(err, socket_path, verb);
    }
    emit(true, &no_server_error(err, socket_path, verb), 1)
}

/// The [`CliError`] behind [`report_no_server`]'s JSON path, also embedded
/// by `phux status --json`.
pub(crate) fn no_server_error(err: &AttachError, socket_path: &Path, verb: &str) -> CliError {
    let mut cli_err = connect_error(
        err,
        socket_path,
        verb,
        crate::commands::diagnostics::recovery_pid(err, socket_path),
    );
    if matches!(err, AttachError::Io(_)) {
        cli_err.auto_start_error = auto_start_failure(socket_path);
        if cli_err.auto_start_error.is_some() {
            cli_err.remedy = format!(
                "auto-starting a server failed: fix the cause in `error.auto_start_error`, \
                 then retry; {}",
                cli_err.remedy
            );
        }
    }
    cli_err
}

/// [`no_server_error`] before any auto-start context is added.
fn connect_error(err: &AttachError, socket_path: &Path, verb: &str, pid: Option<i32>) -> CliError {
    let server_log = phux_config::instance::server_log_path();
    let doctor = format!(
        "server log: {}; run `phux doctor` for a health check",
        server_log.display()
    );
    match err {
        AttachError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
            ) =>
        {
            CliError::new(
                codes::NO_SERVER,
                format!("no server running at {}", socket_path.display()),
                format!(
                    "start one with `phux` (attaches, auto-starting a server) or `phux server`; {doctor}"
                ),
            )
        }
        AttachError::Disconnected => CliError::new(
            codes::SERVER_DISCONNECTED,
            format!("server closed the connection during {verb}"),
            doctor,
        ),
        AttachError::Io(io_err) => CliError::new(
            codes::TRANSPORT,
            unreachable_socket_message(verb, io_err, socket_path),
            doctor,
        ),
        // Same `transport` code as any other non-connect failure. The remedy
        // is the old-server recovery, not a new JSON code.
        AttachError::IncompatibleProtocol(_) => CliError::new(
            codes::TRANSPORT,
            format!("{verb} failed: {err}"),
            crate::commands::diagnostics::old_server_recovery_lines(socket_path, pid).join("\n"),
        ),
        other => CliError::new(codes::TRANSPORT, format!("{verb} failed: {other}"), doctor),
    }
}

/// The sentence for a socket that exists in some form but cannot be dialed
/// (permission denied, not a socket, a path too long for `sockaddr_un`):
/// names the path and the OS reason, never the client's internal "attach
/// loop" wording.
pub(crate) fn unreachable_socket_message(
    verb: &str,
    io_err: &std::io::Error,
    socket_path: &Path,
) -> String {
    format!(
        "{verb}: cannot reach the server socket {}: {io_err}",
        socket_path.display()
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use phux_client::attach::AttachError;

    use super::{codes, error_document, no_server_error};

    /// The three attach-error arms map onto the closed vocabulary, each with
    /// a non-empty remedy.
    #[test]
    fn no_server_errors_use_the_closed_vocabulary() {
        let socket = Path::new("/tmp/phux-test.sock");
        let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let err = no_server_error(&refused, socket, "ls");
        assert_eq!(err.code, codes::NO_SERVER);
        assert!(err.message.contains("/tmp/phux-test.sock"));
        assert!(err.remedy.contains("`phux server`"));
        assert!(err.remedy.contains("phux doctor"));

        let err = no_server_error(&AttachError::Disconnected, socket, "kill");
        assert_eq!(err.code, codes::SERVER_DISCONNECTED);
        assert!(err.message.contains("kill"));
        assert!(!err.remedy.is_empty());

        let err = no_server_error(
            &AttachError::Refused("policy said no".to_owned()),
            socket,
            "tag",
        );
        assert_eq!(err.code, codes::TRANSPORT);
        assert!(err.message.contains("policy said no"));
        assert!(!err.remedy.is_empty());
    }

    /// A failed auto-start for the same socket rides along on the connect
    /// error as the additive `error.auto_start_error`; another socket's does
    /// not, and a disconnect is not a connect failure.
    #[test]
    fn a_recorded_auto_start_failure_rides_on_the_connect_error() {
        let socket = Path::new("/tmp/phux-auto-start-failure-test.sock");
        super::record_auto_start_failure(
            socket,
            &std::io::Error::other("server exited: failed to bind: Permission denied"),
        );
        let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));

        let err = no_server_error(&refused, socket, "new");
        assert_eq!(err.code, codes::NO_SERVER);
        assert!(err.remedy.contains("auto_start_error"));
        let doc = error_document(&err, 1);
        assert_eq!(
            doc["error"]["auto_start_error"],
            "server exited: failed to bind: Permission denied"
        );

        let other = no_server_error(&refused, Path::new("/tmp/elsewhere.sock"), "new");
        assert!(other.auto_start_error.is_none());
        assert!(
            error_document(&other, 1)["error"]
                .get("auto_start_error")
                .is_none()
        );

        let gone = no_server_error(&AttachError::Disconnected, socket, "new");
        assert!(gone.auto_start_error.is_none());
    }

    /// An old-protocol server stays on the existing `transport` code. The
    /// remedy is the stop-and-start recovery, with the pid when it is known.
    #[test]
    fn an_incompatible_server_keeps_transport_and_names_the_stop_recovery() {
        let socket = Path::new("/tmp/old.sock");
        let err = AttachError::IncompatibleProtocol(
            "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
             update the phux server so protocol major.minor match"
                .to_owned(),
        );
        let with_pid = super::connect_error(&err, socket, "ls", Some(4242));
        assert_eq!(with_pid.code, codes::TRANSPORT);
        assert!(
            with_pid.message.contains("server refused the request"),
            "{}",
            with_pid.message
        );
        assert!(with_pid.remedy.contains("kill 4242"), "{}", with_pid.remedy);
        assert!(
            with_pid
                .remedy
                .contains("`ps -p 4242 -o pid=,ppid=,user=,args=`"),
            "{}",
            with_pid.remedy
        );
        assert!(
            with_pid
                .remedy
                .contains("phux server --ensure --socket '/tmp/old.sock'`"),
            "{}",
            with_pid.remedy
        );
        for forbidden in [
            "stale",
            "phux upgrade",
            "survive",
            "remove",
            "pkill",
            "killall",
        ] {
            assert!(
                !with_pid.remedy.contains(forbidden),
                "{forbidden}: {}",
                with_pid.remedy
            );
            assert!(
                !with_pid.message.contains(forbidden),
                "{forbidden}: {}",
                with_pid.message
            );
        }
        let line = serde_json::to_string(&error_document(&with_pid, 1)).unwrap_or_default();
        assert!(!line.contains('\n'), "error line must be single-line");

        let unknown = super::connect_error(&err, socket, "ls", None);
        assert_eq!(unknown.code, codes::TRANSPORT);
        assert!(
            unknown.remedy.contains("`lsof -nP -- '/tmp/old.sock'`"),
            "{}",
            unknown.remedy
        );
        assert!(!unknown.remedy.contains("`kill "), "{}", unknown.remedy);
        for forbidden in [
            "stale",
            "phux upgrade",
            "survive",
            "remove",
            "pkill",
            "killall",
        ] {
            assert!(
                !unknown.remedy.contains(forbidden),
                "{forbidden}: {}",
                unknown.remedy
            );
        }
    }
}
