//! The stable `--json` error contract every surface reports a failure on,
//! the CLI's `--json` verbs and the MCP tools alike (ADR-0065 §4):
//!
//! ```json
//! {"schema_version":1,"error":{"code":"no_server","message":"..."},"remedy":"...","exit_code":1}
//! ```
//!
//! [`Refusal`] is one failure (a stable `code` from [`codes`], the human
//! `message`, the `remedy`), [`error_document`] renders it, and
//! [`transport_refusal`] classifies a local transport failure. Each surface
//! decorates and prints the refusal on its own channel; the CLI's
//! process-global auto-start context stays in the CLI, because a long-lived
//! MCP server would read it stale. Consumers read
//! `docs/scripting.md#json-output-and-errors`.

use std::path::Path;

use crate::attach::AttachError;
use crate::recovery::Recovery;

/// Version of the JSON error document. Additive fields do not bump it.
pub const ERROR_SCHEMA_VERSION: u8 = 1;

/// The closed vocabulary of stable error codes. Consumers branch on these,
/// so renaming one is a breaking change; add new codes here.
#[expect(
    clippy::too_long_first_doc_paragraph,
    reason = "each doc comment is one meaning, catalogued verbatim by `phux --capabilities`"
)]
pub mod codes;

/// One failure, carrying everything every output channel needs: a stable
/// machine `code`, the human `message`, and the `remedy` naming the way out.
#[derive(Debug, Clone)]
pub struct Refusal {
    /// Stable code from [`codes`].
    pub code: &'static str,
    /// What went wrong, for a person.
    pub message: String,
    /// The way out; lines separate independent steps.
    pub remedy: String,
    /// Why this process's auto-start of the server failed, when it tried;
    /// emitted as `error.auto_start_error`.
    pub auto_start_error: Option<String>,
    /// Cold metadata stays off the common error's stack footprint.
    context: Option<Box<RefusalContext>>,
}

#[derive(Debug, Clone, Default)]
struct RefusalContext {
    recovery: Option<Recovery>,
    cause_code: Option<&'static str>,
}

impl Refusal {
    /// A refusal with no recovery, cause code, or auto-start context.
    pub fn new(code: &'static str, message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            remedy: remedy.into(),
            auto_start_error: None,
            context: None,
        }
    }

    /// Attach the typed recovery, emitted as `error.recovery`.
    #[must_use]
    pub fn with_recovery(mut self, recovery: Recovery) -> Self {
        self.context.get_or_insert_with(Box::default).recovery = Some(recovery);
        self
    }

    /// Attach the underlying cause's code, emitted as `error.cause_code`.
    #[must_use]
    pub fn with_cause_code(mut self, code: &'static str) -> Self {
        self.context.get_or_insert_with(Box::default).cause_code = Some(code);
        self
    }
}

/// The JSON error document for `err`. `exit_code` is embedded because a
/// consumer reading stderr may not see the process status.
#[must_use]
pub fn error_document(err: &Refusal, exit_code: u8) -> serde_json::Value {
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

/// How to start a server when none is listening, on every surface.
/// `phux server --ensure` returns once the server is up; bare `phux server`
/// would hold the caller in the foreground.
pub const START_SERVER_REMEDY: &str =
    "start one with `phux` (attaches, auto-starting a server) or `phux server --ensure`";

/// `ConnectionRefused` or `NotFound`: nothing is listening at the socket.
fn is_no_server(err: &AttachError) -> bool {
    matches!(
        err,
        AttachError::Io(io) if matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
        )
    )
}

/// The refusal for a failed exchange with the local server at `socket`.
///
/// `no_server` for a connect failure, `server_disconnected` for a dropped
/// connection, `transport` otherwise. `pid` is the socket owner, used only
/// by the old-protocol recovery. Exit 1 on every arm.
#[must_use]
pub fn transport_refusal(
    err: &AttachError,
    socket: &Path,
    verb: &str,
    pid: Option<i32>,
) -> Refusal {
    let server_log = phux_config::instance::server_log_path();
    let doctor = format!(
        "server log: {}; run `phux doctor` for a health check",
        server_log.display()
    );
    if is_no_server(err) {
        return Refusal::new(
            codes::NO_SERVER,
            format!("no server running at {}", socket.display()),
            format!("{START_SERVER_REMEDY}; {doctor}"),
        );
    }
    match err {
        AttachError::Disconnected => Refusal::new(
            codes::SERVER_DISCONNECTED,
            format!("server closed the connection during {verb}"),
            doctor,
        ),
        AttachError::Io(io_err) => Refusal::new(
            codes::TRANSPORT,
            unreachable_socket_message(verb, io_err, socket),
            doctor,
        ),
        // Same `transport` code as any other non-connect failure. The remedy
        // is the old-server recovery, not a new JSON code.
        AttachError::IncompatibleProtocol(_) => Refusal::new(
            codes::TRANSPORT,
            format!("{verb} failed: {err}"),
            crate::explain::old_server_recovery_lines(socket, pid).join("\n"),
        ),
        other => Refusal::new(codes::TRANSPORT, format!("{verb} failed: {other}"), doctor),
    }
}

/// The refusal for a selector that matched nothing; `target` is `None` for
/// the focused-pane default.
///
/// A whole-fleet miss is `no_such_target`, worded as scripts already grep
/// for. A miss against a partial view never says "no such target", since
/// the pane may be on a satellite this server could not reach: it is
/// `partial_view`, and its remedy names each unreachable satellite.
#[must_use]
pub fn target_miss(target: Option<&str>, degradation: &crate::state::Degradation) -> Refusal {
    if degradation.is_complete() {
        let message = target.map_or_else(
            || "no such target".to_owned(),
            |target| format!("no such target: {target}"),
        );
        return Refusal::new(
            codes::NO_SUCH_TARGET,
            message,
            "run `phux ls` to see live sessions and panes",
        );
    }
    let subject = target.map_or_else(|| "the target".to_owned(), |target| format!("'{target}'"));
    let message = format!(
        "could not resolve {subject}: this server's view of the fleet is incomplete, so a miss \
         here does not mean the target is gone"
    );
    let unreachable: Vec<String> = degradation
        .notices()
        .iter()
        .map(|notice| format!("unreachable — {notice}"))
        .collect();
    let remedy = format!(
        "retry once the satellite link is back; {}",
        unreachable.join("; ")
    );
    Refusal::new(codes::PARTIAL_VIEW, message, remedy)
}

/// The sentence for a socket that exists but cannot be dialed.
///
/// Permission denied, not a socket, a path too long for `sockaddr_un`: it
/// names the path and the OS reason, never the client's internal "attach
/// loop" wording.
#[must_use]
pub fn unreachable_socket_message(verb: &str, io_err: &std::io::Error, socket: &Path) -> String {
    format!(
        "{verb}: cannot reach the server socket {}: {io_err}",
        socket.display()
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{ERROR_SCHEMA_VERSION, Refusal, codes, error_document, transport_refusal};
    use crate::attach::AttachError;

    #[test]
    fn recovery_is_additive_and_never_extracted_from_message_text() {
        use crate::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
        let error = Refusal::new(
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
        let doc = error_document(&error, 1);
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

    /// The emit shape, pinned: `schema_version` 1, nested error object with
    /// code and message, top-level `remedy` and `exit_code`.
    #[test]
    fn error_document_pins_the_contract_shape() {
        let err = Refusal::new(codes::NO_SERVER, "no server running at /tmp/x", "start one");
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

    /// Every attach-error arm maps onto the closed vocabulary, each with a
    /// remedy naming the server log and doctor.
    #[test]
    fn transport_refusals_use_the_closed_vocabulary() {
        let socket = Path::new("/tmp/phux-test.sock");
        let refused = AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let err = transport_refusal(&refused, socket, "ls", None);
        assert_eq!(err.code, codes::NO_SERVER);
        assert!(err.message.contains("/tmp/phux-test.sock"));
        assert!(
            err.remedy.contains("`phux server --ensure`"),
            "{}",
            err.remedy
        );
        assert!(err.remedy.contains("server log: "), "{}", err.remedy);
        assert!(err.remedy.contains("phux doctor"));

        let err = transport_refusal(&AttachError::Disconnected, socket, "kill", None);
        assert_eq!(err.code, codes::SERVER_DISCONNECTED);
        assert!(err.message.contains("kill"));
        assert!(err.remedy.contains("server log: "), "{}", err.remedy);

        let denied = AttachError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let err = transport_refusal(&denied, socket, "tag", None);
        assert_eq!(err.code, codes::TRANSPORT);
        assert!(
            err.message
                .starts_with("tag: cannot reach the server socket /tmp/phux-test.sock: "),
            "{}",
            err.message
        );

        let err = transport_refusal(
            &AttachError::Refused("policy said no".to_owned()),
            socket,
            "tag",
            None,
        );
        assert_eq!(err.code, codes::TRANSPORT);
        assert!(err.message.contains("policy said no"));
        assert!(err.remedy.contains("phux doctor"));
    }

    /// An old-protocol server stays on the existing `transport` code. The
    /// remedy is the stop-and-start recovery, with the pid when it is known.
    #[test]
    fn an_incompatible_server_keeps_transport_and_names_the_stop_recovery() {
        const FORBIDDEN: [&str; 6] = [
            "stale",
            "phux upgrade",
            "survive",
            "remove",
            "pkill",
            "killall",
        ];
        let socket = Path::new("/tmp/old.sock");
        let err = AttachError::IncompatibleProtocol(
            "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
             update the phux server so protocol major.minor match"
                .to_owned(),
        );
        let with_pid = transport_refusal(&err, socket, "ls", Some(4242));
        assert_eq!(with_pid.code, codes::TRANSPORT);
        assert!(
            with_pid.message.contains("server refused the request"),
            "{}",
            with_pid.message
        );
        let remedy = &with_pid.remedy;
        assert!(remedy.contains("kill 4242"), "{remedy}");
        assert!(
            remedy.contains("`ps -p 4242 -o pid=,ppid=,user=,args=`"),
            "{remedy}"
        );
        assert!(
            remedy.contains("phux server --ensure --socket '/tmp/old.sock'`"),
            "{remedy}"
        );
        for forbidden in FORBIDDEN {
            assert!(!remedy.contains(forbidden), "{forbidden}: {remedy}");
            assert!(
                !with_pid.message.contains(forbidden),
                "{forbidden}: {}",
                with_pid.message
            );
        }
        let line = serde_json::to_string(&error_document(&with_pid, 1)).unwrap_or_default();
        assert!(!line.contains('\n'), "error line must be single-line");

        let unknown = transport_refusal(&err, socket, "ls", None);
        assert_eq!(unknown.code, codes::TRANSPORT);
        let remedy = &unknown.remedy;
        assert!(remedy.contains("`lsof -nP -- '/tmp/old.sock'`"), "{remedy}");
        assert!(!remedy.contains("`kill "), "{remedy}");
        for forbidden in FORBIDDEN {
            assert!(!remedy.contains(forbidden), "{forbidden}: {remedy}");
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
