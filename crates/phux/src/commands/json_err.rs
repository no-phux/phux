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
//! The closed `code` vocabulary lives in
//! [`codes`]; consumers read `docs/scripting.md#json-output-and-errors`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use phux_client::attach::AttachError;

/// Version of the JSON error document. Additive fields do not bump it.
pub(crate) const ERROR_SCHEMA_VERSION: u8 = 1;

/// The closed vocabulary of stable error codes. Consumers branch on these,
/// so renaming one is a breaking change; add new codes here.
pub(crate) mod codes;

/// One CLI failure, carrying everything both output channels need: a stable
/// machine `code`, the human `message`, and the `remedy` naming the way out.
#[derive(Debug, Clone)]
pub(crate) struct CliError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) remedy: String,
    /// Why this process's auto-start of the server failed, when it tried;
    /// emitted as `error.auto_start_error`.
    pub(crate) auto_start_error: Option<String>,
    /// Cold metadata stays off the common error's stack footprint.
    context: Option<Box<ErrorContext>>,
}

#[derive(Debug, Clone, Default)]
struct ErrorContext {
    recovery: Option<phux_client::recovery::Recovery>,
    cause_code: Option<&'static str>,
}

impl CliError {
    pub(crate) fn new(
        code: &'static str,
        message: impl Into<String>,
        remedy: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            remedy: remedy.into(),
            auto_start_error: None,
            context: None,
        }
    }
}

impl CliError {
    pub(crate) fn with_recovery(mut self, recovery: phux_client::recovery::Recovery) -> Self {
        self.context.get_or_insert_with(Box::default).recovery = Some(recovery);
        self
    }

    pub(crate) fn with_cause_code(mut self, code: &'static str) -> Self {
        self.context.get_or_insert_with(Box::default).cause_code = Some(code);
        self
    }
}

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

/// The JSON error document for `err`. `exit_code` is embedded because a
/// consumer reading stderr may not see the process status.
pub(crate) fn error_document(err: &CliError, exit_code: u8) -> serde_json::Value {
    let mut error = serde_json::json!({ "code": err.code, "message": err.message });
    if let Some(auto_start_error) = &err.auto_start_error {
        error["auto_start_error"] = serde_json::Value::from(auto_start_error.as_str());
    }
    if let Some(context) = &err.context {
        if let Some(recovery) = &context.recovery {
            error["recovery"] = serde_json::json!(recovery);
        }
        if let Some(code) = context.cause_code {
            error["cause_code"] = serde_json::json!(code);
        }
    }
    serde_json::json!({
        "schema_version": ERROR_SCHEMA_VERSION,
        "error": error,
        "remedy": err.remedy,
        "exit_code": exit_code,
    })
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
    let server_log = phux_server::telemetry::server_log_path();
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
    #[test]
    fn recovery_is_additive_and_never_extracted_from_message_text() {
        use phux_client::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
        let error = super::CliError::new(
            "session_create_failed",
            "opaque diagnostic",
            "inspect state",
        )
        .with_cause_code("outcome_unknown")
        .with_recovery(Recovery::new(
            ScopeKind::OrganizationAuthority,
            Some("actual-operation".into()),
            EffectCertainty::Unknown,
            NextAction::Reconcile,
        ));
        let doc = super::error_document(&error, 1);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["error"]["code"], "session_create_failed");
        assert_eq!(doc["error"]["cause_code"], "outcome_unknown");
        assert_eq!(doc["error"]["recovery"]["operation_id"], "actual-operation");
        assert_eq!(
            doc["error"]["recovery"]["scope"]["kind"],
            "organization_authority"
        );
        assert!(doc["error"]["recovery"]["scope"].get("id").is_none());
    }

    use std::path::Path;

    use phux_client::attach::AttachError;

    use super::{CliError, ERROR_SCHEMA_VERSION, codes, error_document, no_server_error};

    /// The emit shape, pinned: `schema_version` 1, nested error object with
    /// code and message, top-level `remedy` and `exit_code`.
    #[test]
    fn error_document_pins_the_contract_shape() {
        let err = CliError::new(codes::NO_SERVER, "no server running at /tmp/x", "start one");
        let doc = error_document(&err, 1);
        assert_eq!(doc["schema_version"], u64::from(ERROR_SCHEMA_VERSION));
        assert_eq!(doc["error"]["code"], "no_server");
        assert_eq!(doc["error"]["message"], "no server running at /tmp/x");
        assert_eq!(doc["remedy"], "start one");
        assert_eq!(doc["exit_code"], 1);
        // Exactly the four top-level keys; a consumer may deny-list nothing.
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(4));
        // The emitted form is one line.
        let line = serde_json::to_string(&doc).unwrap_or_default();
        assert!(!line.contains('\n'), "error line must be single-line");
    }

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

    /// The agent verbs' codes are part of the closed vocabulary frozen by
    /// ADR-0071 point 6.
    #[test]
    fn the_agent_verb_codes_live_in_the_single_closed_vocabulary() {
        assert_eq!(codes::NO_AGENT_RECORD, "no_agent_record");
        assert_eq!(codes::AGENT_DEPARTED, "agent_departed");
        assert_eq!(codes::AGENT_MISMATCH, "agent_mismatch");
        assert_eq!(codes::INVALID_KEY_SPEC, "invalid_key_spec");
        // Added after ADR-0071 point 6 was written.
        assert_eq!(codes::INPUT_NOT_WRITTEN, "input_not_written");
        assert_eq!(codes::DELIVERY_UNKNOWN, "delivery_unknown");
    }

    /// The agent session family (`agent session open|close`, `agent emit`,
    /// `agent log`) spells its codes here, `snake_case`, so the Claude shim and
    /// the MCP adapter branch on one vocabulary.
    #[test]
    fn the_agent_session_codes_live_in_the_single_closed_vocabulary() {
        assert_eq!(codes::UNSUPPORTED_SERVER, "unsupported_server");
        assert_eq!(codes::NO_AGENT_SESSION, "no_agent_session");
        assert_eq!(codes::AGENT_SESSION_AMBIGUOUS, "agent_session_ambiguous");
        assert_eq!(codes::WRONG_RESOURCE_KIND, "wrong_resource_kind");
        assert_eq!(codes::NOT_PRODUCER, "not_producer");
        assert_eq!(codes::RECORD_INVALID, "record_invalid");
        assert_eq!(codes::OVERFLOW, "overflow");
        assert_eq!(codes::PARENT_NOT_FOUND, "parent_not_found");
        assert_eq!(codes::PARENT_KIND_MISMATCH, "parent_kind_mismatch");
        assert_eq!(codes::AGENT_SESSION_REFUSED, "agent_session_refused");
        assert_eq!(codes::AGENT_WITHDRAWN, "agent_withdrawn");
    }
}
