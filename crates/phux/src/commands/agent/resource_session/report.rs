//! Reading `--data`, and printing documents and session errors.

use super::*;

/// `--data`: inline JSON, `-` for stdin, `{}` when omitted.
pub(super) fn read_data(data: Option<&str>) -> Result<serde_json::Value, AgentSessionError> {
    let text = match data {
        None => return Ok(serde_json::json!({})),
        Some("-") => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|err| AgentSessionError::RecordInvalid(format!("--data - : {err}")))?;
            text
        }
        Some(inline) => inline.to_owned(),
    };
    serde_json::from_str(&text)
        .map_err(|err| AgentSessionError::RecordInvalid(format!("--data is not JSON: {err}")))
}

pub(super) fn print_document(document: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(document) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            true,
            &json_err::CliError::new(
                json_err::codes::JSON_SERIALIZE,
                format!("could not render the document: {err}"),
                "report this: a document of strings and numbers cannot fail to serialize",
            ),
            EXIT_FAILURE,
        ),
    }
}

/// Map a library failure onto the closed error-code vocabulary and exit.
pub(super) fn report_session_error(
    json: bool,
    err: &AgentSessionError,
    socket_path: &Path,
    verb: &str,
) -> ExitCode {
    let (code, exit_code, remedy): (&'static str, u8, String) = match err {
        AgentSessionError::Unsupported => (
            json_err::codes::UNSUPPORTED_SERVER,
            EXIT_USAGE,
            "upgrade the server (`phux upgrade`), or keep using `phux agent set` / \
             `phux agent report-state`; `phux status --json` lists the advertised `features`"
                .to_owned(),
        ),
        AgentSessionError::NoSession { terminal } => (
            json_err::codes::NO_AGENT_SESSION,
            EXIT_USAGE,
            format!(
                "open one with `phux agent session open {} --provider <P>`; `phux ls --json` \
                 lists every resource with its kind and parent",
                phux_client::selector::format_terminal_id(terminal)
            ),
        ),
        AgentSessionError::AmbiguousSession { .. } => (
            json_err::codes::AGENT_SESSION_AMBIGUOUS,
            EXIT_USAGE,
            "address the session directly by its @N".to_owned(),
        ),
        AgentSessionError::WrongKind { expected, .. } => (
            json_err::codes::WRONG_RESOURCE_KIND,
            EXIT_USAGE,
            format!("name a {expected} resource; `phux ls --json` lists every resource's kind"),
        ),
        AgentSessionError::ParentNotFound { .. } => (
            json_err::codes::PARENT_NOT_FOUND,
            EXIT_FAILURE,
            "the pane went away between the lookup and the spawn; re-read `phux ls --json`"
                .to_owned(),
        ),
        AgentSessionError::ParentKindMismatch { .. } => (
            json_err::codes::PARENT_KIND_MISMATCH,
            EXIT_USAGE,
            "name the pane hosting the agent, not another session".to_owned(),
        ),
        AgentSessionError::RecordInvalid(_) => (
            json_err::codes::RECORD_INVALID,
            EXIT_USAGE,
            format!(
                "--type is one of {}; --data is a JSON object under 16 KiB",
                agent_session::EVENT_TYPES.join(", ")
            ),
        ),
        AgentSessionError::NotProducer { .. } => (
            json_err::codes::NOT_PRODUCER,
            EXIT_USAGE,
            "append from a local connection (this machine's socket or localhost); a remote \
             peer cannot feed a session, and an SDK-owned session takes appends only from its \
             native controller"
                .to_owned(),
        ),
        AgentSessionError::Overflow(_) => (
            json_err::codes::OVERFLOW,
            EXIT_USAGE,
            "raise `defaults.agent-log-bytes`, or emit smaller records".to_owned(),
        ),
        AgentSessionError::OutcomeUnknown(_) => (
            json_err::codes::SPAWN_OUTCOME_UNKNOWN,
            EXIT_FAILURE,
            "run `phux ls` to see whether the agent session exists before retrying".to_owned(),
        ),
        AgentSessionError::Refused(_) => (
            json_err::codes::AGENT_SESSION_REFUSED,
            EXIT_FAILURE,
            "run `phux doctor` for a health check".to_owned(),
        ),
        AgentSessionError::Transport(inner) => {
            return json_err::report_no_server(json, inner, socket_path, verb);
        }
    };
    json_err::emit(
        json,
        &json_err::CliError::new(code, format!("{verb}: {err}"), remedy),
        exit_code,
    )
}
