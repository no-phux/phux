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

pub(crate) use phux_client::contract::{
    ERROR_SCHEMA_VERSION, codes, error_document, transport_refusal, unreachable_socket_message,
};

/// One CLI failure: the shared [`phux_client::contract::Refusal`], under the
/// name the CLI's verbs already use.
pub(crate) type CliError = phux_client::contract::Refusal;

/// A verb's failure decided in one place: the error document, the status
/// to exit with, and, when the prose a human reads is not the document's
/// message and remedy, the lines printed instead.
#[derive(Debug)]
pub(crate) struct Refusal {
    /// Boxed so a `Result<_, Refusal>` stays small on the happy path.
    pub(crate) err: Box<CliError>,
    pub(crate) exit: u8,
    /// The human rendering, first line the error and the rest hints, when it
    /// differs from `err`. `None` prints `err` itself.
    human: Option<Vec<String>>,
}

impl Refusal {
    pub(crate) fn new(
        code: &'static str,
        message: impl Into<String>,
        remedy: impl Into<String>,
        exit: u8,
    ) -> Self {
        Self::from_error(CliError::new(code, message, remedy), exit)
    }

    /// Refuse with an error built elsewhere.
    pub(crate) fn from_error(err: CliError, exit: u8) -> Self {
        Self {
            err: Box::new(err),
            exit,
            human: None,
        }
    }

    /// Print `lines` instead of the document without `--json`: the
    /// verb-specific prose a human has always read for this failure.
    pub(crate) fn with_human(mut self, lines: Vec<String>) -> Self {
        self.human = Some(lines);
        self
    }

    /// Attach what a retry needs to know to the error document.
    pub(crate) fn with_recovery(mut self, recovery: phux_client::recovery::Recovery) -> Self {
        self.err = Box::new(self.err.with_recovery(recovery));
        self
    }

    /// The no-server refusal for a failed exchange with the local server at
    /// `socket_path`: [`no_server_error`] under `--json`, the shared
    /// no-server diagnostic otherwise, exit 1.
    pub(crate) fn no_server(err: &AttachError, socket_path: &Path, verb: &str) -> Self {
        Self::from_error(
            no_server_error(err, socket_path, verb),
            crate::exit_codes::EXIT_FAILURE,
        )
        .with_human(crate::commands::diagnostics::report_lines(
            err,
            socket_path,
            verb,
        ))
    }

    /// Report on the verb's own channels and return the exit code.
    pub(crate) fn emit(&self, json: bool) -> ExitCode {
        match (&self.human, json) {
            (Some(lines), false) => {
                crate::output::error_block(lines.iter().map(String::as_str));
                ExitCode::from(self.exit)
            }
            _ => emit(json, &self.err, self.exit),
        }
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
    Refusal::no_server(err, socket_path, verb).emit(json)
}

/// The [`CliError`] behind [`report_no_server`]'s JSON path, also embedded
/// by `phux status --json`.
pub(crate) fn no_server_error(err: &AttachError, socket_path: &Path, verb: &str) -> CliError {
    let mut cli_err = transport_refusal(
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use phux_client::attach::AttachError;

    use super::{codes, error_document, no_server_error};

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
}
