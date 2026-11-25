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
    ///
    /// A message that carries its own remedies as trailing `hint: ` lines
    /// (`phux_config::production::split_hints`) keeps its failure line as
    /// the message; those remedies lead the remedy, one per line.
    pub fn new(code: &'static str, message: impl Into<String>, remedy: impl Into<String>) -> Self {
        let message = message.into();
        let remedy = remedy.into();
        let (failure, hints) = phux_config::production::split_hints(&message);
        let (message, remedy) = if hints.is_empty() {
            (message, remedy)
        } else {
            let mut steps = hints.join("\n");
            if !remedy.is_empty() {
                steps.push('\n');
                steps.push_str(&remedy);
            }
            (failure.to_owned(), steps)
        };
        Self {
            code,
            message,
            remedy,
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

/// [`error_document`] as the one line the CLI prints on stderr, keys in the
/// documented order (`schema_version`, `error`, `remedy`, `exit_code`)
/// whatever `serde_json` features the build unified.
#[must_use]
pub fn error_line(err: &Refusal, exit_code: u8) -> String {
    let context = err.context.as_deref();
    let line = ErrorLine {
        schema_version: ERROR_SCHEMA_VERSION,
        error: ErrorBody {
            code: err.code,
            message: &err.message,
            auto_start_error: err.auto_start_error.as_deref(),
            recovery: context.and_then(|context| context.recovery.as_ref()),
            cause_code: context.and_then(|context| context.cause_code),
        },
        remedy: &err.remedy,
        exit_code,
    };
    // Strings, numbers, and a derived struct cannot fail to serialize; the
    // unordered document keeps the failure visible rather than silent.
    serde_json::to_string(&line).unwrap_or_else(|_| error_document(err, exit_code).to_string())
}

/// The top level of [`error_line`], in document order.
#[derive(serde::Serialize)]
struct ErrorLine<'a> {
    schema_version: u8,
    error: ErrorBody<'a>,
    remedy: &'a str,
    exit_code: u8,
}

/// The `error` object of [`error_line`], in document order.
#[derive(serde::Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_start_error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<&'a Recovery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cause_code: Option<&'a str>,
}

/// How to start a server when none is listening, on every surface.
/// `phux server --ensure` returns once the server is up; bare `phux server`
/// would hold the caller in the foreground.
pub const START_SERVER_REMEDY: &str =
    "start one with `phux` (attaches, auto-starting a server) or `phux server --ensure`";

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
    if err.is_no_server() {
        return Refusal::new(
            codes::NO_SERVER,
            format!("no server running at {}", socket.display()),
            format!("{START_SERVER_REMEDY}; {doctor}"),
        );
    }
    if let AttachError::Io(io_err) = err
        && err.is_timeout()
    {
        return Refusal::new(
            codes::TIMEOUT,
            format!("{verb}: {io_err}"),
            format!(
                "raise PHUX_TIMEOUT (seconds; 0 waits forever) if the server is only slow; {doctor}"
            ),
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

/// The refusal for a target in a session waiting for `phux restore`.
///
/// A server restart ended its panes and nothing has started them again
/// ([`crate::org::OrgInventory::awaiting_restore`]). Still `no_such_target`,
/// so scripts that match the code keep working.
#[must_use]
pub fn awaiting_restore(target: &str, session: &str) -> Refusal {
    let restore = if session.contains(char::is_whitespace) {
        format!("phux restore '{session}'")
    } else {
        format!("phux restore {session}")
    };
    Refusal::new(
        codes::NO_SUCH_TARGET,
        format!(
            "no such target: {target}: session {session} is waiting for restore \
             (a server restart ended its panes)"
        ),
        format!("run `{restore}` to start its panes again, then retry"),
    )
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
mod tests;
