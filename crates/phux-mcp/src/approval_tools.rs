//! `phux_approvals` / `phux_approve` (ADR-0128): list the actions a
//! `?signal` grant holds for a decision, and decide one. Approving releases
//! the held action, so it needs `confirm: true`; a denial never asks.

use phux_client::approvals::{ApprovalError, Decision};
use phux_client::attach::connection::Connection;
use phux_protocol::ids::ApprovalId;
use serde_json::{Value, json};

use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, socket_or_default, str_arg, strict_object};

pub(crate) fn schemas() -> Vec<Value> {
    vec![approvals_schema(), approve_schema()]
}

pub(crate) fn owns(name: &str) -> bool {
    matches!(name, "phux_approvals" | "phux_approve")
}

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
        "Decide one held action. `approve` releases it: the server runs it once, as the workload that asked, under that workload's own grant. `deny` refuses it and nothing runs. Deciding needs the held verbs (`signal`, `terminate`, or `input`) un-held on the held action's subject. Approving requires confirm=true; a denial never does. Credential requests may be denied but require a supported interactive review client to approve.",
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
    Ok(Connection::connect(&socket_or_default(args)).await?)
}

fn approval_error(err: ApprovalError) -> ToolError {
    match err {
        ApprovalError::Transport(err) => err.into(),
        ApprovalError::Unsupported => crate::tools::contract_error(
            "unsupported_server",
            err.to_string(),
            "use a server that advertises approvals",
            2,
        ),
        other => crate::tools::contract_error(
            "approval_refused",
            other.to_string(),
            "use a caller with inventory and observe on Global approval records; deciding additionally requires the held verbs un-held on the subject",
            1,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::wire::frame::FrameKind;

    #[tokio::test]
    async fn mcp_approval_connections_do_not_claim_interactive_credential_review() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("approval.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = ScriptedServer::accept(&listener, ScriptSpec::new());
        let client = async {
            let conn = connect(&json!({ "socket": socket })).await.unwrap();
            conn.shutdown().await;
        };
        let (seen, ()) = tokio::join!(server, client);
        assert!(matches!(
            seen.as_slice(),
            [FrameKind::Hello { client_caps, .. }] if !client_caps.credential_approvals
        ));
    }
}
