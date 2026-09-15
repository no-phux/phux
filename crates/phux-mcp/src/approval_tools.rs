//! The `phux_approvals` and `phux_approve` tools (ADR-0128): list the
//! actions a `?signal` grant is holding for a decision, and decide one.
//!
//! In-process over `phux_client::approvals`, returning the document
//! `phux approvals --json` prints, so the two surfaces cannot drift.
//! Approving releases a held `SIGNAL` action, so `phux_approve` is
//! destructive and the shared `confirm` check
//! ([`crate::annotations::require_confirmation`]) asks for `confirm: true`
//! before an approval; a denial releases nothing and never asks.

use phux_client::approvals::{ApprovalError, Decision};
use phux_client::attach::connection::Connection;
use phux_protocol::ids::ApprovalId;
use serde_json::{Value, json};

use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, str_arg, strict_object};

/// The two approval tool descriptors.
pub(crate) fn schemas() -> Vec<Value> {
    vec![approvals_schema(), approve_schema()]
}

/// Whether `name` is one of this module's tools.
pub(crate) fn owns(name: &str) -> bool {
    matches!(name, "phux_approvals" | "phux_approve")
}

/// Dispatch one approval tool call.
pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "phux_approvals" => approvals(args).await,
        "phux_approve" => approve(args).await,
        other => Err(ToolError::new(format!("unknown approval tool: {other}"))),
    }
}

fn approvals_schema() -> Value {
    schema(
        "phux_approvals",
        "List the actions the server is holding for approval: kills, signals, and forced detaches from a workload whose grant holds `?signal`. Each entry has the approval `id`, the `requester`, the held `method` (and `signal`), its `subjects`, and `expires_at_ms`. Read-only; the document `phux approvals --json` prints.",
        json!({ "socket": string_schema() }),
        &[],
    )
}

fn approve_schema() -> Value {
    schema(
        "phux_approve",
        "Decide one held action. `approve` releases it: the server runs it once, as the workload that asked, under that workload's own grant. `deny` refuses it and nothing runs. Deciding needs un-held `signal` on the held action's subject. Approving requires confirm=true; a denial never does.",
        json!({
            "id": {
                "type": "string",
                "pattern": "^[0-9a-f]{32}$",
                "description": "The approval id phux_approvals lists.",
            },
            "decision": { "type": "string", "enum": ["approve", "deny"] },
            "confirm": {
                "type": "boolean",
                "description": "Required true to approve: it releases a held destructive action.",
            },
            "socket": string_schema(),
        }),
        &["id", "decision"],
    )
}

async fn approvals(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["socket"], &[])?;
    let listed = {
        let mut conn = connect(args).await?;
        phux_client::approvals::list(&mut conn).await
    };
    let approvals = listed.map_err(approval_error)?;
    Ok(json!({ "schema_version": 1, "approvals": approvals }))
}

async fn approve(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["id", "decision", "confirm", "socket"],
        &["id", "decision"],
    )?;
    let id = str_arg(args, "id")
        .and_then(ApprovalId::parse)
        .ok_or_else(|| ToolError::new("`id` must be the 32-hex-digit id phux_approvals lists"))?;
    let decision = match str_arg(args, "decision") {
        Some("approve") => Decision::Approve,
        Some("deny") => Decision::Deny,
        _ => return Err(ToolError::new("`decision` must be `approve` or `deny`")),
    };
    let decided = {
        let mut conn = connect(args).await?;
        phux_client::approvals::decide(&mut conn, id, decision).await
    };
    decided.map_err(approval_error)?;
    Ok(json!({
        "schema_version": 1,
        "id": id.to_string(),
        "decision": decision.as_str(),
    }))
}

async fn connect(args: &Value) -> Result<Connection, ToolError> {
    let socket = crate::socket::resolve(str_arg(args, "socket"));
    Ok(Connection::connect(&socket).await?)
}

fn approval_error(err: ApprovalError) -> ToolError {
    match err {
        ApprovalError::Transport(err) => err.into(),
        other => ToolError::new(other.to_string()),
    }
}
