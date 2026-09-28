//! `phux_ask`: report that an agent in a pane is asking for human input.

use phux_client::ask::AskedPayload;
use phux_client::selector;
use phux_client::state;
use phux_protocol::ids::ResourceId;
use serde_json::{Value, json};

use crate::tools::{
    ToolError, num_arg, optional_string_array, parse_selector, required_str, resolve_one,
    socket_or_default,
};

pub(crate) async fn call(args: &Value) -> Result<Value, ToolError> {
    let socket = socket_or_default(args);
    let selector = parse_selector(required_str(args, "target")?)?;
    let view = state::get_state(&socket).await?;
    let pane = resolve_one(&socket, &selector, &view).await?;
    let payload = AskedPayload {
        id: required_str(args, "id")?.to_owned(),
        question: required_str(args, "question")?.to_owned(),
        suggestions: optional_string_array(args, "suggestions")?.unwrap_or_default(),
        elapsed_seconds: num_arg(args, "elapsed_seconds"),
    };

    phux_client::ask::report(&socket, pane.clone(), payload.clone()).await?;
    Ok(success_value(&pane, &payload))
}

pub(crate) fn schema() -> Value {
    json!({
        "name": "phux_ask",
        "description": "Report that an agent in a pane is asking for human input, emitting the same asked event phux watch observes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "Target selector: session, session:window, session:window.pane, @paneid, host/@paneid, or `.` for the focused session. `=` is unsupported because MCP has no attached-client focus history." },
                "id": { "type": "string", "description": "Stable question id for answer correlation." },
                "question": { "type": "string", "description": "Human-facing question text." },
                "suggestions": { "type": "array", "items": { "type": "string" }, "description": "Suggested answers in display order." },
                "elapsed_seconds": { "type": "number", "description": "Seconds the agent has already been waiting." },
                "socket": { "type": "string" }
            },
            "required": ["target", "id", "question"]
        }
    })
}

fn success_value(pane: &ResourceId, payload: &AskedPayload) -> Value {
    json!({
        "schema_version": 1,
        "event": "asked",
        "terminal": selector::format_terminal_id(pane),
        "id": payload.id,
        "question": payload.question,
        "suggestions": payload.suggestions,
        "elapsed_seconds": payload.elapsed_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satellite_success_output_uses_canonical_selector() {
        let payload = AskedPayload {
            id: "q1".to_owned(),
            question: "Continue?".to_owned(),
            suggestions: vec!["yes".to_owned()],
            elapsed_seconds: Some(5),
        };
        let value = success_value(&ResourceId::satellite("region/@build", 7), &payload);
        assert_eq!(value["terminal"], json!("region/@build/@7"));
        assert_eq!(value["event"], json!("asked"));
        assert_eq!(value["id"], json!("q1"));
        assert_eq!(value["schema_version"], json!(1));
    }

    #[test]
    fn suggestions_default_to_absent_and_reject_non_strings() {
        let parse = |args: Value| optional_string_array(&args, "suggestions");
        assert_eq!(parse(json!({})).unwrap(), None);
        assert_eq!(
            parse(json!({ "suggestions": ["yes", "no"] })).unwrap(),
            Some(vec!["yes".to_owned(), "no".to_owned()])
        );
        assert!(parse(json!({ "suggestions": [1] })).is_err());
    }
}
