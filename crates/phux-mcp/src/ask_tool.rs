use phux_client::ask::AskedPayload;
use phux_client::selector::{self, Selector};
use phux_client::state::{self, StateView};
use phux_protocol::ids::TerminalId;
use serde_json::{Value, json};

use crate::socket;
use crate::tools::ToolError;

pub(crate) async fn call(args: &Value) -> Result<Value, ToolError> {
    let socket = socket::resolve(str_arg(args, "socket"));
    let target = required_str(args, "target")?;
    let selector = selector::parse(target)
        .map_err(|err| ToolError::new(format!("invalid target '{target}': {err}")))?;
    let view = state::get_state(&socket).await?;
    let pane = resolve_one(&socket, &selector, &view).await?;
    let payload = AskedPayload {
        id: required_str(args, "id")?.to_owned(),
        question: required_str(args, "question")?.to_owned(),
        suggestions: string_array_opt(args, "suggestions")?.unwrap_or_default(),
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

fn success_value(pane: &TerminalId, payload: &AskedPayload) -> Value {
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

/// Resolve `selector` to one pane, distinguishing "no such target" from "I
/// could not see all of the fleet" — the same split `tools::resolve_one`
/// makes, and for the same reason: a hub that could not reach a satellite
/// still answers `GET_STATE`, minus that satellite's panes.
async fn resolve_one(
    socket: &std::path::Path,
    selector: &Selector,
    view: &StateView,
) -> Result<TerminalId, ToolError> {
    let snapshot = view.snapshot();
    let candidates = state::resolve_targets(socket, selector, snapshot).await;
    selector::pick_target_pane(&candidates, &snapshot.focused_pane).ok_or_else(|| {
        if view.is_complete() {
            ToolError::new("no such target")
        } else {
            ToolError::new(format!(
                "could not resolve the target: this server's view of the fleet is \
                 incomplete ({}), so a miss here does not mean the target is gone",
                view.degradation().notices().join("; ")
            ))
        }
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    str_arg(args, key).ok_or_else(|| ToolError::new(format!("missing required string `{key}`")))
}

fn num_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn string_array_opt(args: &Value, key: &str) -> Result<Option<Vec<String>>, ToolError> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let arr = value
        .as_array()
        .ok_or_else(|| ToolError::new(format!("`{key}` must be an array of strings")))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| ToolError::new(format!("`{key}` must contain only strings")))
        })
        .collect::<Result<Vec<String>, _>>()
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_requires_target_id_and_question() {
        let schema = schema();
        assert_eq!(schema["name"], json!("phux_ask"));
        assert_eq!(
            schema["inputSchema"]["required"],
            json!(["target", "id", "question"])
        );
        assert_eq!(
            schema["inputSchema"]["properties"]["suggestions"]["items"]["type"],
            json!("string")
        );
        assert!(
            schema["inputSchema"]["properties"]["target"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("host/@paneid"))
        );
    }

    #[test]
    fn satellite_success_output_uses_canonical_selector() {
        let payload = AskedPayload {
            id: "q1".to_owned(),
            question: "Continue?".to_owned(),
            suggestions: vec!["yes".to_owned()],
            elapsed_seconds: Some(5),
        };
        let value = success_value(&TerminalId::satellite("region/@build", 7), &payload);
        assert_eq!(value["terminal"], json!("region/@build/@7"));
        assert_eq!(value["event"], json!("asked"));
        assert_eq!(value["id"], json!("q1"));
        assert_eq!(value["schema_version"], json!(1));
    }

    /// The result document is a frozen surface under ADR-0071, so it carries
    /// a version. Pinned separately from the selector test above so a future
    /// edit to the payload cannot quietly drop it.
    #[test]
    fn success_output_is_versioned() {
        let payload = AskedPayload {
            id: "q1".to_owned(),
            question: "Continue?".to_owned(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        };
        let value = success_value(&TerminalId::local(3), &payload);
        assert_eq!(value["schema_version"], json!(1));
    }

    #[test]
    fn suggestions_default_to_absent_and_reject_non_strings() {
        assert_eq!(string_array_opt(&json!({}), "suggestions").unwrap(), None);
        assert_eq!(
            string_array_opt(&json!({ "suggestions": ["yes", "no"] }), "suggestions").unwrap(),
            Some(vec!["yes".to_owned(), "no".to_owned()])
        );
        assert!(string_array_opt(&json!({ "suggestions": [1] }), "suggestions").is_err());
    }
}
