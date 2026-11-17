use serde_json::{Value, json};

use phux_client::agent::detect::agents_document;
use phux_client::agent::reads::{self, ListFilter, ReadError};
use phux_client::contract::{self, Refusal};
use phux_client::selector::Selector;

use super::{DECLARED_STATES, TARGET_DESC, target_schema};
use crate::cli_adapter::{bounded_string, bounded_strings};
use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, cli_refusal_error, socket_arg, strict_object, transport_refusal_at};

/// The CLI's status for a whole-fleet miss.
const EXIT_FAILURE: u8 = 1;

/// The CLI's status for a miss against a partial fleet view.
const EXIT_PARTIAL_VIEW: u8 = 3;

// -----------------------------------------------------------------------------
// list / show / explain — the level reads.
// -----------------------------------------------------------------------------

pub(super) fn list_schema() -> Value {
    schema(
        "phux_agent_list",
        "List every pane's projected agent state (declared phux.agent/v1 record where present, \
         otherwise the server's detector). This is a LEVEL read: it asserts only the absence of \
         contrary evidence, so a pane reported `idle` may equally be finished, crashed, or \
         running a program with no detection manifest. Use phux_agent_wait for a completion gate. \
         Optional states are ORed; query words are ANDed across inventory fields, never terminal contents.",
        json!({
            "socket": string_schema(),
            "states": { "type": "array", "maxItems": 64,
                "items": { "type": "string", "enum": DECLARED_STATES },
                "description": "Include only these current states; omit or empty for all." },
            "query": { "type": "string", "minLength": 1, "maxLength": 4096,
                "description": "Case-insensitive words across pane address, session, agent kind/name, title and cwd." },
        }),
        &[],
    )
}

pub(super) fn show_schema() -> Value {
    schema(
        "phux_agent_show",
        "Show the projected agent state for one pane. LEVEL read — see phux_agent_list for what \
         that does and does not assert.",
        json!({ "target": target_schema(TARGET_DESC), "socket": string_schema() }),
        &[],
    )
}

pub(super) fn explain_schema() -> Value {
    schema(
        "phux_agent_explain",
        "Show one pane's projected agent state together with the evidence behind it: every \
         detection source that fired, its confidence, and what it observed. Use this when a \
         state looks wrong. The offline capture-file mode of the CLI verb is deliberately not \
         exposed here — it is a manifest-authoring debugger over a local file, not a \
         control-plane read.",
        json!({ "target": target_schema(TARGET_DESC), "socket": string_schema() }),
        &[],
    )
}

pub(super) async fn list(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["socket", "states", "query"], &[])?;
    let states = bounded_strings(args, "states", false)?;
    if states
        .iter()
        .any(|state| !DECLARED_STATES.contains(&state.as_str()))
    {
        return Err(ToolError::new(format!(
            "`states` must contain only: {}",
            DECLARED_STATES.join(", ")
        )));
    }
    let query = bounded_string(args, "query", false)?;
    let socket = socket_arg(args)?;
    let filter = ListFilter {
        states: &states,
        query: query.as_deref(),
    };
    let projection = reads::list(&socket, &filter, &mut Vec::new())
        .await
        .map_err(|err| read_error(&err, None, &socket, "agent list"))?;
    Ok(agents_document(&projection.reports))
}

/// `phux_agent_show` and `phux_agent_explain`: one document, since the
/// `--json` view of both verbs is the same report.
pub(super) async fn projection(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &[])?;
    let socket = socket_arg(args)?;
    let target = bounded_string(args, "target", false)?;
    let selector = cli_selector(target.as_deref())?;
    let projection = reads::show(&socket, &selector, &mut Vec::new())
        .await
        .map_err(|err| read_error(&err, target.as_deref(), &socket, "agent"))?;
    Ok(agents_document(&projection.reports))
}

/// `target` parsed as `phux agent show` parses it. That verb reports a
/// malformed selector as its human error line even under `--json`, and the
/// tool has always carried that line as its error text.
fn cli_selector(target: Option<&str>) -> Result<Selector, ToolError> {
    let Some(target) = target else {
        return Ok(Selector::Current);
    };
    phux_client::selector::parse(target)
        .map_err(|err| ToolError::new(format!("phux: error: invalid target '{target}': {err}")))
}

/// A level read's failure as the error document the CLI prints for it.
fn read_error(
    err: &ReadError,
    target: Option<&str>,
    socket: &std::path::Path,
    verb: &str,
) -> ToolError {
    match err {
        ReadError::Transport(err) => {
            cli_refusal_error(&transport_refusal_at(err, socket, verb), EXIT_FAILURE)
        }
        ReadError::Resolve(err) => cli_refusal_error(
            &Refusal::new(err.code(), err.to_string(), err.remedy()),
            err.exit_code(),
        ),
        ReadError::Miss(degradation) => {
            let exit = if degradation.is_complete() {
                EXIT_FAILURE
            } else {
                EXIT_PARTIAL_VIEW
            };
            cli_refusal_error(&contract::target_miss(target, degradation), exit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{assert_argv, fake_cli};
    use super::*;

    /// `phux_agent_send_keys` executes the exact canonical argv, with the
    /// `--` separator ahead of the caller-supplied positionals.
    #[tokio::test]
    async fn send_keys_executes_the_exact_canonical_argv() {
        let (_temp, adapter, log) = fake_cli();
        assert_argv(
            &adapter,
            &log,
            "phux_agent_send_keys",
            json!({
                "target": "@7", "keys": ["yes", "Enter"],
                "expect_agent": "bot", "expect_kind": "codex", "socket": "/sock"
            }),
            &[
                "agent",
                "send-keys",
                "--json",
                "--expect-agent",
                "bot",
                "--expect-kind",
                "codex",
                "--socket",
                "/sock",
                "--",
                "@7",
                "yes",
                "Enter",
            ],
        )
        .await;
    }

    /// Invalid filters are refused before the socket is dialed: each is an
    /// argument error, not the dead socket's `no_server`.
    #[tokio::test]
    async fn list_rejects_invalid_filters_before_dialing() {
        for mut args in [
            json!({ "states": ["sleeping"] }),
            json!({ "states": "blocked" }),
            json!({ "states": [false] }),
            json!({ "states": vec!["idle"; 65] }),
            json!({ "query": false }),
            json!({ "query": "" }),
            json!({ "query": "x".repeat(4097) }),
        ] {
            args["socket"] = json!("/nonexistent/phux-mcp-agent-list.sock");
            let err = list(&args).await.expect_err("an invalid filter");
            assert!(!err.0.contains("no_server"), "{args}: {}", err.0);
        }
    }
}
