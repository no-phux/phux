/// A tool-level failure: surfaced to the caller as a `tools/call` result
/// with `isError: true`, never as a process crash.
#[derive(Clone, Debug)]
pub(crate) struct ToolError(pub(crate) String, Value);

impl ToolError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        let document = serde_json::from_str::<Value>(&message)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({ "schema_version": 1, "error": { "code": "tool_error", "message": message } }));
        Self(message, document)
    }

    pub(crate) const fn with_document(message: String, document: Value) -> Self {
        Self(message, document)
    }

    pub(crate) fn document(&self) -> Value {
        self.1.clone()
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<AttachError> for ToolError {
    fn from(err: AttachError) -> Self {
        Self::new(err.to_string())
    }
}

fn kill_error(err: phux_client::kill::KillError) -> ToolError {
    use phux_client::kill::KillError;
    match err {
        KillError::Unresolved {
            target,
            degradation,
        } => incomplete_view_miss(&format!("'{target}'"), degradation.notices()),
        KillError::Agent(err) => agent_resolve_error(&err),
        other => ToolError::new(other.to_string()),
    }
}

/// A miss against a degraded fleet view: the target may exist on a
/// satellite that could not be reached, so it is not "no such target".
pub(crate) fn incomplete_view_miss(subject: &str, notices: &[String]) -> ToolError {
    ToolError::new(format!(
        "could not resolve {subject}: this server's view of the fleet is incomplete ({}), so a \
         miss here does not mean the target is gone",
        notices.join("; ")
    ))
}

/// A `%name` refusal on the `--json` error contract, with the code and exit
/// status the CLI reports it with (ADR-0075 point 3).
pub(crate) fn agent_resolve_error(err: &selector::AgentResolveError) -> ToolError {
    contract_error(err.code(), err.to_string(), err.remedy(), err.exit_code())
}

/// A refusal on the CLI's one-line `--json` error contract, so the tool
/// error carries the stable `code` a caller branches on.
pub(crate) fn contract_error(
    code: &'static str,
    message: impl Into<String>,
    remedy: &str,
    exit_code: u8,
) -> ToolError {
    refusal_error(&Refusal::new(code, message, remedy), exit_code)
}

/// `refusal` rendered as the shared [`error_document`], the tool error's
/// text and its structured document alike.
pub(crate) fn refusal_error(refusal: &Refusal, exit_code: u8) -> ToolError {
    let document = error_document(refusal, exit_code);
    ToolError::with_document(document.to_string(), document)
}

/// `refusal` as a tool that left the CLI residue reports it: the text is
/// the CLI's own stderr line ([`phux_client::contract::error_line`]), so a
/// caller that read the CLI's error text reads the same bytes.
pub(crate) fn cli_refusal_error(refusal: &Refusal, exit_code: u8) -> ToolError {
    ToolError::with_document(
        phux_client::contract::error_line(refusal, exit_code),
        error_document(refusal, exit_code),
    )
}

/// A transport failure on the `--json` error contract, classified by the
/// same [`transport_refusal`] as the CLI: `no_server`,
/// `server_disconnected`, or `transport`.
pub(crate) fn transport_error(err: &AttachError, socket: &Path, verb: &str) -> ToolError {
    render_transport_error(err, socket, verb, recovery_pid(err, socket))
}

/// The [`transport_refusal`] the CLI builds for `err`, with the server pid
/// its old-protocol recovery names.
pub(crate) fn transport_refusal_at(err: &AttachError, socket: &Path, verb: &str) -> Refusal {
    transport_refusal(err, socket, verb, recovery_pid(err, socket))
}

/// The socket owner's pid, looked up only for the old-protocol recovery.
fn recovery_pid(err: &AttachError, socket: &Path) -> Option<i32> {
    phux_client::attach::is_incompatible_protocol(err)
        .then(|| phux_client::explain::server_pid(socket))
        .flatten()
}

/// [`transport_error`] with the server pid already resolved, so tests can pin
/// both the known-pid and unknown-pid recoveries.
pub(crate) fn render_transport_error(
    err: &AttachError,
    socket: &Path,
    verb: &str,
    pid: Option<i32>,
) -> ToolError {
    refusal_error(&transport_refusal(err, socket, verb, pid), 1)
}
