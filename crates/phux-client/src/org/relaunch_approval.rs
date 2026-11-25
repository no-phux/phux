//! Exact raw archived recipes require a separate, visible owner decision.
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::native_agent::{NativeRequest, NativeRequestPayload, relaunch::RelaunchApproval};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};
use crate::attach::connection::Connection;
use super::{OrgError, OrgOutcome};

fn command(request: NativeRequest) -> Result<Command, OrgError> {
    NativeRequestPayload::new(request).map(|request| Command::NativeAgent { request })
        .map_err(OrgError::Unexpected)
}

fn failure(result: CommandResult) -> OrgError {
    match result {
        CommandResult::Refused(refusal) => OrgError::Refused(refusal),
        CommandResult::Error { code, message } => OrgError::Server { code, message },
        other => OrgError::Unexpected(format!("invalid raw relaunch reply: {other:?}")),
    }
}

/// Read an inert proposal. Display the entire returned value before asking for
/// explicit approval. Keep this connection: proposals cannot cross connections.
/// No execution or persistent trust is created by this call.
///
/// # Errors
/// Refused for remote/delegated principals or stale records; unsupported peers
/// cannot be bypassed by falling back to an ordinary raw spawn.
pub async fn prepare_relaunch(
    conn: &mut Connection, operation: OperationId, resource: ResourceKey, expected: Revision,
) -> Result<RelaunchApproval, OrgError> {
    let request = command(NativeRequest::PrepareRelaunch { operation, resource, expected })?;
    let (reply, _) = conn.request(0x6fa0, request).await.map_err(OrgError::Attach)?.into_parts();
    match reply {
        CommandResult::OkWith(CommandValue::Json(json)) => {
            let proposal: RelaunchApproval = serde_json::from_str(&json)
                .map_err(|error| OrgError::Unexpected(error.to_string()))?;
            if proposal.operation != operation || proposal.resource != resource || proposal.expected != expected {
                return Err(OrgError::Unexpected("raw relaunch proposal targets a different intent".into()));
            }
            Ok(proposal)
        }
        other => Err(failure(other)),
    }
}

/// Send exactly the proposal explicitly reviewed and approved by the owner.
/// Never refreshes it, retries a conflict, or reconnects to approve silently.
/// Accepted means admission only, not that an executable successfully started.
///
/// # Errors
/// Unknown transport outcomes preserve the original operation for reconciliation.
pub async fn approve_relaunch(
    conn: &mut Connection, approval: RelaunchApproval,
) -> Result<OrgOutcome, OrgError> {
    let operation = approval.operation;
    let request = command(NativeRequest::ApproveRelaunch { approval })?;
    let (reply, _) = conn.request(0x6fa1, request).await
        .map_err(|_| OrgError::OutcomeUnknown { operation: Some(operation) })?.into_parts();
    match reply {
        CommandResult::Accepted { operation: acknowledged } if acknowledged == operation => Ok(OrgOutcome::Accepted),
        CommandResult::OkWith(CommandValue::Committed(receipt)) if receipt.operation == operation => Ok(OrgOutcome::Committed(receipt)),
        other => Err(failure(other)),
    }
}
