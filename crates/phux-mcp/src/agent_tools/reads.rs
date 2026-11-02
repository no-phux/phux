use serde_json::{Value, json};

use super::{DECLARED_STATES, TARGET_DESC, push_target, target_schema};
use crate::cli_adapter::{
    CliAdapter, DEFAULT_CALL_TIMEOUT, bounded_string, bounded_strings, push_socket,
};
use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, strict_object};

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

pub(super) async fn list(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
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
    let mut argv = vec!["agent".to_owned(), "list".to_owned(), "--json".to_owned()];
    for state in states {
        argv.extend(["--state".to_owned(), state]);
    }
    if let Some(query) = bounded_string(args, "query", false)? {
        // Keep even a query starting with `--` a value, never another flag.
        argv.push(format!("--query={query}"));
    }
    push_socket(&mut argv, args)?;
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

pub(super) async fn projection(
    args: &Value,
    verb: &str,
    adapter: &CliAdapter,
) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &[])?;
    let mut argv = vec!["agent".to_owned(), verb.to_owned(), "--json".to_owned()];
    push_socket(&mut argv, args)?;
    push_target(&mut argv, args)?;
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

#[cfg(test)]
mod tests {
    use super::super::{
        call_with_adapter,
        tests::{assert_argv, fake_cli},
    };
    use super::*;

    /// Every tool executes the exact canonical argv, with the `--`
    /// separator ahead of any caller-supplied positional.
    #[tokio::test]
    async fn every_agent_tool_executes_the_exact_canonical_argv() {
        let (_temp, adapter, log) = fake_cli();

        assert_argv(
            &adapter,
            &log,
            "phux_agent_list",
            json!({ "socket": "/sock" }),
            &["agent", "list", "--json", "--socket", "/sock"],
        )
        .await;

        assert_argv(
            &adapter,
            &log,
            "phux_agent_list",
            json!({ "states": ["blocked", "done"], "query": "mini backend" }),
            &[
                "agent",
                "list",
                "--json",
                "--state",
                "blocked",
                "--state",
                "done",
                "--query=mini backend",
            ],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_agent_list",
            json!({ "query": "--state" }),
            &["agent", "list", "--json", "--query=--state"],
        )
        .await;

        assert_argv(
            &adapter,
            &log,
            "phux_agent_show",
            json!({ "target": "@5", "socket": "/sock" }),
            &["agent", "show", "--json", "--socket", "/sock", "--", "@5"],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_agent_explain",
            json!({}),
            &["agent", "explain", "--json"],
        )
        .await;

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

    /// Invalid filters are refused before any subprocess runs.
    #[tokio::test]
    async fn list_rejects_invalid_filters_before_invoking_cli() {
        let (_temp, adapter, log) = fake_cli();
        for args in [
            json!({ "states": ["sleeping"] }),
            json!({ "states": "blocked" }),
            json!({ "states": [false] }),
            json!({ "states": vec!["idle"; 65] }),
            json!({ "query": false }),
            json!({ "query": "" }),
            json!({ "query": "x".repeat(4097) }),
        ] {
            assert!(
                call_with_adapter("phux_agent_list", &args, &adapter)
                    .await
                    .is_err(),
                "{args}"
            );
        }
        assert!(!log.exists(), "invalid filters must never run the CLI");
    }
}
