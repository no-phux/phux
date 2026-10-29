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
        KillError::UnsupportedKeyedSignal => crate::pane_tools::unsupported_keyed_signal(),
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
    code: &str,
    message: impl Into<String>,
    remedy: &str,
    exit_code: u8,
) -> ToolError {
    let document = json!({
        "schema_version": 1,
        "error": { "code": code, "message": message.into() },
        "remedy": remedy,
        "exit_code": exit_code,
    });
    ToolError::new(document.to_string())
}

/// A transport failure on the `--json` error contract, with the codes the
/// CLI uses for one: `no_server`, `server_disconnected`, or `transport`.
pub(crate) fn transport_error(err: &AttachError, socket: &Path, verb: &str) -> ToolError {
    let pid = phux_client::attach::is_incompatible_protocol(err)
        .then(|| phux_client::explain::server_pid(socket))
        .flatten();
    render_transport_error(err, socket, verb, pid)
}

/// [`transport_error`] with the server pid already resolved, so tests can pin
/// both the known-pid and unknown-pid recoveries.
pub(crate) fn render_transport_error(
    err: &AttachError,
    socket: &Path,
    verb: &str,
    pid: Option<i32>,
) -> ToolError {
    const DOCTOR: &str = "run `phux doctor` for a health check";
    match err {
        AttachError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            contract_error(
                "no_server",
                format!("no server running at {}", socket.display()),
                "start one with `phux` (attaches, auto-starting a server) or `phux server`; \
                 run `phux doctor` for a health check",
                1,
            )
        }
        AttachError::Disconnected => contract_error(
            "server_disconnected",
            format!("server closed the connection during {verb}"),
            DOCTOR,
            1,
        ),
        AttachError::IncompatibleProtocol(_) => contract_error(
            "transport",
            format!("{verb} failed: {err}"),
            &phux_client::explain::old_server_recovery_lines(socket, pid).join("\n"),
            1,
        ),
        other => contract_error("transport", format!("{verb} failed: {other}"), DOCTOR, 1),
    }
}
