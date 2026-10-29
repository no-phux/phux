//! Thin MCP adapters for the same public intention and server description as the CLI.
#[cfg(test)]
mod tests;

use crate::tools::{ToolError, socket_arg, strict_object};
use phux_protocol::api::TerminalCreate;
use serde_json::{Value, json};

pub(crate) fn schemas() -> Vec<Value> {
    let mut input = phux_protocol::api::terminal_create_schema();
    input["properties"]["socket"] = json!({"type":"string"});
    let mut lookup = phux_protocol::api::terminal_operation_schema();
    lookup["properties"]["socket"] = json!({"type":"string"});
    vec![
        json!({"name":"phux_terminal_operation","description":"Read a retained creation receipt by its original session and idempotency_key. Never executes, retries, or starts a process. Missing receipt is an unknown original outcome, not proof it never ran. Use this after lost replies instead of blind replay.","inputSchema":lookup}),
        json!({"name":"phux_terminal_create","description":"Create, place and execute argv as one intention. Requires a full durable session key and nonzero idempotency_key. Retains failed/completed execution until explicit removal. Commitment is not process/task completion. Use phux_api_describe for connected-server schemas and recovery rules.","inputSchema":input}),
        json!({"name":"phux_api_describe","description":"Read the connected server's supported public workflows, complete nested schemas and retry/completion/authority contracts. Support does not imply authorization.","inputSchema":{"type":"object","properties":{"socket":{"type":"string"}},"additionalProperties":false}}),
    ]
}

pub(crate) fn description_schema() -> Value {
    json!({"type":"object","required":["schema_version","source","authority","instance","operations","authorization"],"properties":{"schema_version":{"const":1},"source":{"const":"connected_server"},"authority":{"type":"string"},"instance":{"type":"string"},"authorization":{"type":"string"},"operations":{"type":"array","items":{"type":"object","required":["name","supported","input_schema","output_schema","retry"],"properties":{"name":{"type":"string"},"supported":{"type":"boolean"},"input_schema":{"type":"object"},"output_schema":{"type":"object"},"retry":{"type":"object"}}}}}})
}

pub(crate) async fn create(args: &Value) -> Result<Value, ToolError> {
    let socket = socket_arg(args)?;
    let mut request = args.clone();
    if let Some(object) = request.as_object_mut() {
        object.remove("socket");
    }
    let request: TerminalCreate =
        serde_json::from_value(request).map_err(|error| ToolError::new(error.to_string()))?;
    phux_client::terminal::create_on(&socket, &request)
        .await
        .map(|result| json!(result))
        .map_err(|error| {
            ToolError::with_document(error.to_string(), error.document(&request.idempotency_key))
        })
}

pub(crate) async fn operation(args: &Value) -> Result<Value, ToolError> {
    let socket = socket_arg(args)?;
    let mut request = args.clone();
    if let Some(object) = request.as_object_mut() {
        object.remove("socket");
    }
    let request: phux_protocol::api::TerminalOperation =
        serde_json::from_value(request).map_err(|error| ToolError::new(error.to_string()))?;
    phux_client::terminal::operation_on(&socket, &request)
        .await
        .map(|result| json!(result))
        .map_err(|error| {
            ToolError::with_document(error.to_string(), error.document(&request.idempotency_key))
        })
}

pub(crate) async fn describe(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["socket"], &[])?;
    let socket = socket_arg(args)?;
    phux_client::terminal::describe_on(&socket)
        .await
        .map_err(|error| ToolError::new(error.to_string()))
}
