use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
use phux_client::attach::connection::Connection;
use phux_client::selector::{Selector, format_terminal_id};
use phux_protocol::ids::ResourceId;
use serde_json::{Value, json};

use super::{ATTENTION_LEVELS, DECLARED_STATES, TARGET_DESC, target_schema};
use crate::cli_adapter::{bounded_string, enum_string};
use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, parse_selector, resolve_one, socket_arg, strict_object};

// -----------------------------------------------------------------------------
// set / clear — the declared record.
// -----------------------------------------------------------------------------

pub(super) fn set_schema() -> Value {
    schema(
        "phux_agent_set",
        "Declare a pane's agent identity by writing its phux.agent/v1 L3 record. Only `name` is \
         required. A declared `state` OUTRANKS the server's detector for the record's lifetime, \
         so declare one only when the caller genuinely knows better than the detector does.",
        json!({
            "target": target_schema(TARGET_DESC),
            "name": string_schema(),
            "kind": string_schema(),
            "state": { "type": "string", "enum": DECLARED_STATES },
            "attention": { "type": "string", "enum": ATTENTION_LEVELS },
            "session": string_schema(),
            "socket": string_schema(),
        }),
        &["name"],
    )
}

pub(super) fn clear_schema() -> Value {
    schema(
        "phux_agent_clear",
        "Delete a pane's declared phux.agent/v1 record. The pane's state reverts to whatever the \
         server's detector projects; anything waiting on that agent sees a departure, not a \
         completion.",
        json!({
            "target": target_schema(TARGET_DESC),
            "socket": string_schema(),
        }),
        &[],
    )
}

/// `phux_agent_set` — write the whole `phux.agent/v1` record (last writer
/// wins) and return it as the server confirmed it, in-process.
pub(super) async fn set(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &[
            "target",
            "name",
            "kind",
            "state",
            "attention",
            "session",
            "socket",
        ],
        &["name"],
    )?;
    let record = declared_record(args)?;
    let socket = socket_arg(args)?;
    let (mut conn, pane) = target_pane(&socket, args).await?;
    // The trailing GET inside `set_record` is load-bearing: SET_METADATA has
    // no reply frame, so the confirmed value is what the server read back.
    let (answer, _) = phux_client::agent_record::set_record(&mut conn, 100, &pane, &record).await?;
    drop(conn);
    match answer {
        Ok(Some(confirmed)) => record_document("set", &pane, Some(&confirmed)),
        Ok(None) => Err(ToolError::new("agent record did not persist")),
        Err(refusal) => Err(ToolError::new(format!(
            "agent record could not be confirmed: {refusal}"
        ))),
    }
}

/// The record `set` declares, validated the way `phux agent set` validates
/// its flags.
fn declared_record(args: &Value) -> Result<AgentRecord, ToolError> {
    let state = args
        .get("state")
        .map(|_| enum_string(args, "state", DECLARED_STATES, None))
        .transpose()?;
    let attention = args
        .get("attention")
        .map(|_| enum_string(args, "attention", ATTENTION_LEVELS, None))
        .transpose()?;
    let name = bounded_string(args, "name", true)?.unwrap_or_default();
    if name.trim().is_empty() {
        return Err(ToolError::new("`name` must not be empty"));
    }
    Ok(AgentRecord {
        name: name.trim().to_owned(),
        kind: bounded_string(args, "kind", false)?,
        state: state.map(AgentMetaState::from).unwrap_or_default(),
        attention: attention.map(AgentAttention::from),
        session: bounded_string(args, "session", false)?,
    })
}

/// `phux_agent_clear` — delete the record and confirm it is gone.
pub(super) async fn clear(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &[])?;
    let socket = socket_arg(args)?;
    let (mut conn, pane) = target_pane(&socket, args).await?;
    let (answer, _) = phux_client::agent_record::clear_record(&mut conn, 100, &pane).await?;
    drop(conn);
    match answer {
        Ok(None) => record_document("clear", &pane, None),
        Ok(Some(_)) => Err(ToolError::new("agent record was not cleared")),
        Err(refusal) => Err(ToolError::new(format!(
            "agent clear could not be confirmed: {refusal}"
        ))),
    }
}

/// Connect and resolve the optional `target` (the focused pane when absent)
/// against that connection's own snapshot, as `phux agent set` / `clear` do.
async fn target_pane(
    socket: &std::path::Path,
    args: &Value,
) -> Result<(Connection, ResourceId), ToolError> {
    let selector = bounded_string(args, "target", false)?
        .map(|raw| parse_selector(&raw))
        .transpose()?
        .unwrap_or(Selector::Current);
    let mut conn = Connection::connect(socket).await?;
    let view = phux_client::state::get_state_on(&mut conn).await?;
    let pane = resolve_one(socket, &selector, &view).await?;
    Ok((conn, pane))
}

/// The versioned `{schema_version, action, terminal, record}` document;
/// `record` is `null` for a cleared record.
fn record_document(
    action: &str,
    pane: &ResourceId,
    record: Option<&AgentRecord>,
) -> Result<Value, ToolError> {
    let record = match record {
        None => Value::Null,
        Some(record) => serde_json::from_slice(&record.encode())
            .map_err(|err| ToolError::new(format!("agent record is malformed JSON: {err}")))?,
    };
    Ok(json!({
        "schema_version": 1,
        "action": action,
        "terminal": format_terminal_id(pane),
        "record": record,
    }))
}
