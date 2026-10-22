//! MCP-specific contracts layered over the canonical CLI documents.
use crate::tools::ToolError;
use serde_json::{Value, json};

pub(crate) fn operation_status_schema() -> Value {
    crate::cli_tools::schema(
        "phux_operation_status",
        "Observe a keyed mutation in this MCP adapter: pending, completed with result, or failed with a structured error. Pending operations may need phux_approvals and a separate authorized phux_approve caller. No waiting or execution. The ledger does not survive adapter restart.",
        json!({ "idempotency_key": key_schema() }),
        &["idempotency_key"],
    )
}

fn key_schema() -> Value {
    json!({ "type": "string", "minLength": 32, "maxLength": 32, "pattern": "^[0-9a-fA-F]{32}$", "description": "Nonzero 32-hex-digit retry key. Same tool and arguments replay one invocation during this adapter's lifetime (128 keys, no eviction). Not durable across reconnect/restart. phux_new/spawn/kill/signal additionally forward server-backed keys. Reconcile state before retrying after a disconnect." })
}

#[allow(
    clippy::expect_used,
    reason = "the compiled schema file is validated in contract tests"
)]
pub(crate) fn enhance(catalog: &mut Value) {
    let Some(entries) = catalog.as_array_mut() else {
        return;
    };
    let outputs = output_schemas();
    for entry in entries {
        let name = entry["name"].as_str().unwrap_or_default().to_owned();
        let mut output = outputs[&name].clone();
        if crate::operations::supports_mutation(&name) {
            let properties = &mut entry["inputSchema"]["properties"];
            if properties.get("idempotency_key").is_none() {
                properties["idempotency_key"] = key_schema();
            }
            entry["inputSchema"]["if"] = json!({ "properties": { "background": { "const": true } }, "required": ["background"] });
            entry["inputSchema"]["then"] = json!({ "required": ["idempotency_key"] });
            entry["inputSchema"]["properties"]["background"] = json!({ "type": "boolean", "description": "Return an operation receipt immediately instead of waiting (including server approval holds). Requires idempotency_key. Observe with phux_operation_status; cancellation only ends observation. Adapter exit loses the ledger and cancels unfinished work; effects may already have happened." });
            output = json!({ "type": "object", "anyOf": [output, operation_output()] });
        }
        entry["outputSchema"] = output;
        add_resource_id(&mut entry["inputSchema"]);
    }
}

/// Every tool's output schema, by tool name, from one file per tool family.
#[allow(
    clippy::expect_used,
    reason = "the compiled schema files are validated in contract tests"
)]
pub(crate) fn output_schemas() -> Value {
    let families = [
        include_str!("outputs.json"),
        include_str!("agent_outputs.json"),
        include_str!("host_outputs.json"),
        include_str!("coord_outputs.json"),
    ];
    let mut schemas = serde_json::Map::new();
    for family in families {
        let family: serde_json::Map<String, Value> =
            serde_json::from_str(family).expect("compiled output schemas");
        schemas.extend(family);
    }
    Value::Object(schemas)
}

fn add_resource_id(input: &mut Value) {
    let Some(target) = input["properties"].get_mut("target") else {
        return;
    };
    target["description"] = json!(
        "Prefer a direct resource ID returned by inventory (@N or host/@N), or use resource_id. Selector strings remain supported for compatibility; names, tags, and focus can resolve differently over time."
    );
    input["properties"]["resource_id"] = json!({ "type": "string", "minLength": 2, "maxLength": 4096, "description": "Exact resource ID (@N or host/@N). Preferred over target selectors; mutually exclusive with target. IDs belong to one server incarnation." });
    input["properties"]["resource_id"]["pattern"] = json!("^(.+/)?@[0-9]+$");
    input["not"] = json!({ "required": ["target", "resource_id"] });
    let Some(required) = input.get_mut("required").and_then(Value::as_array_mut) else {
        return;
    };
    if required.iter().any(|key| key == "target") {
        required.retain(|key| key != "target");
        input["anyOf"] = json!([{ "required": ["target"] }, { "required": ["resource_id"] }]);
    }
}

pub(crate) fn normalize_target(mut args: Value) -> Result<Value, ToolError> {
    let object = args
        .as_object_mut()
        .ok_or_else(|| ToolError::new("tool arguments must be an object"))?;
    let Some(value) = object.remove("resource_id") else {
        return Ok(args);
    };
    if object.contains_key("target") {
        return Err(ToolError::new("`resource_id` conflicts with `target`"));
    }
    let raw = value
        .as_str()
        .filter(|raw| raw.len() <= 4096)
        .ok_or_else(|| ToolError::new("`resource_id` must be an exact resource ID"))?;
    if crate::tools::parse_selector(raw)?.explicit_id().is_none() {
        return Err(ToolError::new(
            "`resource_id` must be @N or host/@N, not a selector",
        ));
    }
    object.insert("target".to_owned(), value);
    Ok(args)
}

/// Preserve the CLI document while exposing a directly reusable resource ID.
pub(crate) fn result_document(mut value: Value) -> Value {
    let Some(object) = value.as_object_mut() else {
        return json!({ "result": value });
    };
    let direct = ["resource", "terminal", "pane", "terminal_id"]
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_str))
        .find(|raw| {
            phux_client::selector::parse(raw)
                .ok()
                .and_then(|selector| selector.explicit_id())
                .is_some()
        })
        .map(str::to_owned);
    let resource_id = direct.or_else(|| {
        let id = object
            .get("terminal_id")?
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())?;
        let id = object.get("satellite").and_then(Value::as_str).map_or_else(
            || phux_protocol::ids::ResourceId::local(id),
            |host| phux_protocol::ids::ResourceId::satellite(host, id),
        );
        Some(phux_client::selector::format_terminal_id(&id))
    });
    if let Some(id) = resource_id {
        object.insert("resource_id".to_owned(), json!(id));
    }
    value
}

pub(crate) fn operation_output() -> Value {
    json!({ "type": "object", "required": ["schema_version", "operation_id", "state"], "properties": {
        "schema_version": { "type": "integer", "const": 1 },
        "operation_id": { "type": "string" },
        "state": { "type": "string", "enum": ["pending", "completed", "failed"] },
        "result": {}, "error": { "type": "object" },
        "status_tool": { "type": "string" }, "approval_check": { "type": "object" },
        "request": { "type": "object", "properties": { "tool": { "type": "string" }, "arguments": { "type": "object" } } }
    } })
}
