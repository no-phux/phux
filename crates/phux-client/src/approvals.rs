//! Server-held approvals (ADR-0128, `docs/spec/workload-auth.md` §6.1):
//! list the actions awaiting a decision, and decide one.
//!
//! The pending set is the server-owned `phux.approval/v1/<id>` records
//! under `Global` (`docs/spec/L3.md` §3.10). A decision is a `SET_METADATA`
//! of `phux.approval.decide/v1/<id>` with `approve` or `deny`, which the
//! server intercepts and never stores. `SET_METADATA` has no reply, so a
//! decision is confirmed by reading its record back on the same connection:
//! the server handles one connection's frames in order, so a refused
//! decision's correlated `ERROR` arrives before the read's answer.

use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::ApprovalId;
use phux_protocol::wire::frame::{APPROVAL_KEY_PREFIX, FrameKind, Scope};

use crate::attach::AttachError;
use crate::attach::connection::{Connection, Refusal};

use phux_client_runtime::approvals::DecisionOutcome;
/// The record and the decision are the runtime's one parse, shared with the
/// control plane's live projection.
pub use phux_client_runtime::approvals::{Approval, Decision, Requester};

/// The correlation id of a decision's `SET_METADATA`.
const DECIDE_ID: u32 = 1;

/// The correlation id of the read that confirms a decision.
const CONFIRM_ID: u32 = 2;

/// The correlation id of the key listing.
const LIST_ID: u32 = 1;

/// Why listing or deciding failed.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    /// The server does not advertise `APPROVALS`: it holds nothing, and it
    /// would store a decision as an ordinary value.
    #[error("the server does not hold actions for approval (it predates approvals)")]
    Unsupported,
    /// The server refused the request.
    #[error("{0}")]
    Refused(Refusal),
    /// A record did not parse, or a decided record is still pending.
    #[error("unexpected approval state: {0}")]
    Malformed(String),
    /// The exchange with the server failed.
    #[error(transparent)]
    Transport(#[from] AttachError),
}

/// Every action awaiting a decision on the server behind `conn`, oldest
/// first. A record decided between the listing and its read is skipped.
///
/// # Errors
///
/// [`ApprovalError::Unsupported`] before sending anything to a server
/// without the feature; a refusal, an unparsable record, or a transport
/// failure otherwise.
pub async fn list(conn: &mut Connection) -> Result<Vec<Approval>, ApprovalError> {
    require_approvals(conn).await?;
    let (answer, _interleaved) = conn
        .request_metadata_keys(LIST_ID, Scope::Global)
        .await?
        .into_parts();
    let keys = answer.map_err(ApprovalError::Refused)?;
    let mut approvals = Vec::new();
    for (index, key) in keys
        .into_iter()
        .filter(|key| key.starts_with(APPROVAL_KEY_PREFIX))
        .enumerate()
    {
        let request_id = u32::try_from(index).map_or(u32::MAX, |i| i.saturating_add(LIST_ID + 1));
        if let Some(approval) = read_record(conn, request_id, key).await? {
            approvals.push(approval);
        }
    }
    approvals.sort_by_key(|approval| approval.requested_at_ms);
    Ok(approvals)
}

async fn read_record(
    conn: &mut Connection,
    request_id: u32,
    key: String,
) -> Result<Option<Approval>, ApprovalError> {
    let (answer, _interleaved) = conn
        .request_metadata(request_id, Scope::Global, key)
        .await?
        .into_parts();
    let Some(bytes) = answer.map_err(ApprovalError::Refused)? else {
        return Ok(None);
    };
    Approval::parse(&bytes)
        .map(Some)
        .map_err(|err| ApprovalError::Malformed(format!("approval record: {err}")))
}

/// Decide the held action `id` names.
///
/// # Errors
///
/// [`ApprovalError::Unsupported`] before sending anything to a server
/// without the feature; [`ApprovalError::Refused`] when the server refused
/// the decision (no such pending approval, or no un-held `SIGNAL` on its
/// subject); a transport failure otherwise.
pub async fn decide(
    conn: &mut Connection,
    id: ApprovalId,
    decision: Decision,
) -> Result<(), ApprovalError> {
    require_approvals(conn).await?;
    conn.send(&FrameKind::SetMetadata {
        request_id: DECIDE_ID,
        scope: Scope::Global,
        key: id.decide_key(),
        value: decision.value().to_vec(),
    })
    .await?;
    let (answer, interleaved) = conn
        .request_metadata(CONFIRM_ID, Scope::Global, id.record_key())
        .await?
        .into_parts();
    let refusal = decision_refusal(&interleaved);
    // A decider without OBSERVE on Global cannot read the record; its
    // refusal of the read says nothing about the decision (`None`).
    let still_pending = answer.ok().map(|record| record.is_some());
    let outcome = DecisionOutcome::classify(
        refusal
            .as_ref()
            .map(|refusal| (refusal.code, refusal.message.clone())),
        still_pending,
    );
    match (outcome, refusal) {
        (DecisionOutcome::Decided, _) => Ok(()),
        // Stale or refused: report the server's own refusal.
        (_, Some(refusal)) => Err(ApprovalError::Refused(refusal)),
        (_, None) => Err(ApprovalError::Malformed(format!(
            "approval {id} is still pending after the decision"
        ))),
    }
}

/// The correlated refusal of the decision, if the server sent one.
fn decision_refusal(frames: &[FrameKind]) -> Option<Refusal> {
    frames.iter().find_map(|frame| match frame {
        FrameKind::Error {
            request_id: Some(DECIDE_ID),
            code,
            message,
        } => Some(Refusal {
            code: *code,
            message: message.clone(),
        }),
        _ => None,
    })
}

async fn require_approvals(conn: &mut Connection) -> Result<(), ApprovalError> {
    let features = crate::state::probe_hello_features(conn).await?;
    if features.is_some_and(|features| features.contains(ServerFeature::Approvals)) {
        Ok(())
    } else {
        Err(ApprovalError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::wire::frame::ErrorCode;

    #[test]
    fn only_the_decisions_own_correlated_error_refuses_it() {
        let error = |request_id| FrameKind::Error {
            request_id,
            code: ErrorCode::PermissionDenied,
            message: "permission denied".to_owned(),
        };
        assert!(decision_refusal(&[error(None), error(Some(CONFIRM_ID))]).is_none());
        assert_eq!(
            decision_refusal(&[error(Some(DECIDE_ID))]).map(|refusal| refusal.code),
            Some(ErrorCode::PermissionDenied)
        );
    }
}
