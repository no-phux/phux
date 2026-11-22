use phux_client::native_agent::questions;
use serde_json::{Value, json};

use crate::cli_adapter::bounded_string;
use crate::cli_tools::{schema as tool_schema, string_schema};
use crate::tools::{ToolError, socket_arg, strict_object};

pub(super) fn schema() -> Value {
    tool_schema(
        "phux_agent_questions",
        "Read the server's current questions for one exact resource. Structured callbacks and \
         advisory PTY evidence are distinct. This tool cannot answer questions or grant approval.",
        json!({
            "target": super::target_schema("Exact local @N or AUTHORITY/^RESOURCE."),
            "socket": string_schema(),
        }),
        &["target"],
    )
}

pub(super) async fn current(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &["target"])?;
    let target = bounded_string(args, "target", true)?
        .as_deref()
        .and_then(questions::target)
        .ok_or_else(|| ToolError::new("target must be an exact local @N or AUTHORITY/^RESOURCE"))?;
    let socket = socket_arg(args)?;
    let document = questions::current(&socket, target)
        .await
        .map_err(|error| ToolError::new(error.to_string()))?;
    serde_json::to_value(document).map_err(|error| ToolError::new(error.to_string()))
}
