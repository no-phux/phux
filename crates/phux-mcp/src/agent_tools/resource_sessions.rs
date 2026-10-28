use serde_json::{Value, json};

use super::target_schema;
use crate::cli_adapter::{
    CliAdapter, DEFAULT_CALL_TIMEOUT, bounded_string, enum_string, push_socket,
};
use crate::cli_tools::{push_option, schema, string_schema};
use crate::tools::{ToolError, strict_object};

// -----------------------------------------------------------------------------
// session open / close, emit, log — the AgentSession resource.
// -----------------------------------------------------------------------------

/// The closed `AgentEventsJsonlV1` record `type` vocabulary, checked before
/// a subprocess runs (the CLI and server enforce the same set).
const EVENT_TYPES: &[&str] = &[
    "session_start",
    "prompt",
    "tool_start",
    "tool_end",
    "notification",
    "ask",
    "stop",
    "session_end",
    "state",
    "provider_raw",
];

/// Largest `tail` `phux_agent_log` forwards: a tool result is one text block.
const LOG_MAX_TAIL: u64 = 10_000;

/// Shared `target` description for the session verbs.
const SESSION_TARGET_DESC: &str = "Target selector: the pane hosting the agent (`@N`, `%name`, \
    `session:group.pane`) resolves to its unique live AgentSession child; an AgentSession \
    resource id (`@N`) names itself. Required.";

pub(super) fn session_open_schema() -> Value {
    schema(
        "phux_agent_session_open",
        "Open an AgentSession resource bound to a pane: the server-side, producer-fed event \
         log an agent harness appends to with phux_agent_emit. The session is a child of the \
         Terminal it names — closing the pane closes it — and it is what `%name` targets, \
         phux_agent_log reads, and the sidebar shows under the pane. Refused with \
         `unsupported_server` on a server that does not advertise `resource_kinds` (check \
         phux_status's `features`). Returns {schema_version, resource, parent, provider, \
         native_id}.",
        json!({
            "target": target_schema("The parent pane: a selector resolving to one Terminal-kind resource. Required."),
            "provider": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Agent provider slug, e.g. `claude`." },
            "native_id": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Opaque provider-native session id, when the provider has one." },
            "socket": string_schema(),
        }),
        &["target", "provider"],
    )
}

pub(super) fn session_close_schema() -> Value {
    schema(
        "phux_agent_session_close",
        "Close a pane's AgentSession resource. Closing a child never affects the parent pane. \
         Returns {schema_version, resource, closed}.",
        json!({
            "target": target_schema(SESSION_TARGET_DESC),
            "socket": string_schema(),
        }),
        &["target"],
    )
}

pub(super) fn emit_schema() -> Value {
    schema(
        "phux_agent_emit",
        "Append one AgentEventsJsonlV1 record to a pane's AgentSession log. The server stamps \
         `seq` and `ts_ms`; the caller supplies `type` (closed vocabulary) and an optional \
         `data` object (default `{}`). The server derives the agent's lifecycle state from the \
         stream (`prompt`/`tool_start` -> working, `ask` -> blocked, `stop` -> done, \
         `session_end` -> retract). Returns the stamped header {schema_version, resource, \
         seq, ts_ms, type}.",
        json!({
            "target": target_schema(SESSION_TARGET_DESC),
            "type": { "type": "string", "enum": EVENT_TYPES, "description": "Record type." },
            "data": { "type": "object", "description": "Record payload, a JSON object. Omit for `{}`." },
            "socket": string_schema(),
        }),
        &["target", "type"],
    )
}

pub(super) fn log_schema() -> Value {
    schema(
        "phux_agent_log",
        "Read the retained AgentEventsJsonlV1 records of a pane's AgentSession: one bounded \
         read of the server-side ring, never a live follow (a tool result is one text block; \
         the CLI's `phux agent log --follow` is the streaming form). Returns \
         {schema_version, resource, parent, provider, native_id, records: [{seq, ts_ms, \
         type, data}]}.",
        json!({
            "target": target_schema(SESSION_TARGET_DESC),
            "tail": { "type": "number", "minimum": 1, "maximum": LOG_MAX_TAIL, "description": "Return only the last N records. Omit for every retained record." },
            "socket": string_schema(),
        }),
        &["target"],
    )
}

pub(super) async fn session_open(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "provider", "native_id", "socket"],
        &["target", "provider"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let provider = bounded_string(args, "provider", true)?.unwrap_or_default();
    let mut argv = vec![
        "agent".to_owned(),
        "session".to_owned(),
        "open".to_owned(),
        "--provider".to_owned(),
        provider,
    ];
    push_option(
        &mut argv,
        "--native-id",
        bounded_string(args, "native_id", false)?,
    );
    argv.push("--json".to_owned());
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

pub(super) async fn session_close(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &["target"])?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let mut argv = vec!["agent".to_owned(), "session".to_owned(), "close".to_owned()];
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    // `session close` has no `--json`: it prints `@N<TAB>closed`.
    let output = adapter.run(argv, DEFAULT_CALL_TIMEOUT).await?;
    parse_closed_line(&output.stdout)
}

/// Parse the `@N<TAB>closed` line `phux agent session close` prints into the
/// small documented projection.
fn parse_closed_line(stdout: &str) -> Result<Value, ToolError> {
    let line = stdout.trim();
    match line.split_once('\t') {
        Some((resource, "closed")) if !resource.is_empty() => {
            Ok(json!({ "schema_version": 1, "resource": resource, "closed": true }))
        }
        _ => Err(ToolError::new(format!(
            "phux agent session close returned malformed output: {line:?}"
        ))),
    }
}

pub(super) async fn emit(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "type", "data", "socket"],
        &["target", "type"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let event_type = enum_string(args, "type", EVENT_TYPES, None)?;
    let data = match args.get("data") {
        None => None,
        Some(Value::Object(object)) => Some(
            serde_json::to_string(&Value::Object(object.clone()))
                .map_err(|err| ToolError::new(format!("`data` could not be encoded: {err}")))?,
        ),
        Some(_) => return Err(ToolError::new("`data` must be a JSON object")),
    };
    let mut argv = vec![
        "agent".to_owned(),
        "emit".to_owned(),
        "--type".to_owned(),
        event_type,
    ];
    push_option(&mut argv, "--data", data);
    argv.push("--json".to_owned());
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

pub(super) async fn log(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["target", "tail", "socket"], &["target"])?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let tail = match args.get("tail") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|value| (1..=LOG_MAX_TAIL).contains(value))
                .ok_or_else(|| {
                    ToolError::new(format!("`tail` must be an integer in 1..={LOG_MAX_TAIL}"))
                })?,
        ),
    };
    let mut argv = vec!["agent".to_owned(), "log".to_owned(), "--json".to_owned()];
    push_option(&mut argv, "--tail", tail.map(|tail| tail.to_string()));
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    // Never `--follow`: a tool result is one bounded text block.
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

#[cfg(test)]
mod tests {
    use super::super::{
        call_with_adapter,
        tests::{assert_argv, fake_cli},
    };
    use super::*;

    /// The session verbs execute the exact canonical argv: flags first, the
    /// `--` separator, then the caller's selector.
    #[tokio::test]
    async fn session_tools_execute_the_exact_canonical_argv() {
        let (_temp, adapter, log) = fake_cli();

        let opened = assert_argv(
            &adapter,
            &log,
            "phux_agent_session_open",
            json!({ "target": "@7", "provider": "claude", "native_id": "abc-123", "socket": "/sock" }),
            &[
                "agent", "session", "open", "--provider", "claude", "--native-id", "abc-123",
                "--json", "--socket", "/sock", "--", "@7",
            ],
        )
        .await;
        assert_eq!(opened["resource"], "@9");
        assert_eq!(opened["parent"], "@7");

        let closed = assert_argv(
            &adapter,
            &log,
            "phux_agent_session_close",
            json!({ "target": "%reviewer" }),
            &["agent", "session", "close", "--", "%reviewer"],
        )
        .await;
        assert_eq!(closed["resource"], "@9");
        assert_eq!(closed["closed"], true);

        let emitted = assert_argv(
            &adapter,
            &log,
            "phux_agent_emit",
            json!({ "target": "@7", "type": "prompt", "data": { "length": 4 }, "socket": "/sock" }),
            &[
                "agent",
                "emit",
                "--type",
                "prompt",
                "--data",
                "{\"length\":4}",
                "--json",
                "--socket",
                "/sock",
                "--",
                "@7",
            ],
        )
        .await;
        assert_eq!(emitted["seq"], 42);
        assert_eq!(emitted["type"], "prompt");

        // No `data` means no `--data`: the CLI defaults the payload to `{}`.
        assert_argv(
            &adapter,
            &log,
            "phux_agent_emit",
            json!({ "target": "@9", "type": "stop" }),
            &["agent", "emit", "--type", "stop", "--json", "--", "@9"],
        )
        .await;

        let logged = assert_argv(
            &adapter,
            &log,
            "phux_agent_log",
            json!({ "target": "@7", "tail": 50, "socket": "/sock" }),
            &[
                "agent", "log", "--json", "--tail", "50", "--socket", "/sock", "--", "@7",
            ],
        )
        .await;
        assert_eq!(logged["schema_version"], 1);
        assert_eq!(logged["resource"], "@9");
        assert_eq!(logged["parent"], "@7");
        assert_eq!(logged["records"][1]["type"], "prompt");
        assert_eq!(logged["records"][1]["data"]["chars"], 4);
    }

    /// Session-verb argument validation happens before any subprocess.
    #[tokio::test]
    async fn session_tool_arguments_are_validated_before_execution() {
        let adapter = CliAdapter::new("must-not-execute");
        for (name, args) in [
            ("phux_agent_session_open", json!({ "target": "@7" })),
            ("phux_agent_session_open", json!({ "provider": "claude" })),
            ("phux_agent_session_close", json!({})),
            ("phux_agent_emit", json!({ "target": "@7" })),
            (
                "phux_agent_emit",
                json!({ "target": "@7", "type": "frobnicate" }),
            ),
            (
                "phux_agent_emit",
                json!({ "target": "@7", "type": "stop", "data": "text" }),
            ),
            ("phux_agent_log", json!({})),
            ("phux_agent_log", json!({ "target": "@7", "tail": 0 })),
            ("phux_agent_log", json!({ "target": "@7", "follow": true })),
        ] {
            assert!(
                call_with_adapter(name, &args, &adapter).await.is_err(),
                "{name} accepted {args}",
            );
        }
    }

    #[test]
    fn the_closed_line_parser_is_strict() {
        let doc = parse_closed_line("@9\tclosed\n").unwrap();
        assert_eq!(doc["resource"], "@9");
        assert_eq!(doc["closed"], true);
        assert!(parse_closed_line("@9\tgone").is_err());
        assert!(parse_closed_line("closed").is_err());
        assert!(parse_closed_line("").is_err());
    }
}
