//! Nonsecret approval views shared by the C and `UniFFI` encoders.

use phux_client_runtime::approvals::{Approval, DecisionOutcome};
use phux_client_runtime::control::Event;
use serde::Serialize;

/// One confirmed, refused, stale, or unconfirmed decision. Never retry automatically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalDecision {
    /// Immutable server approval id.
    pub id: String,
    /// `approve` or `deny`.
    pub decision: String,
    /// `decided`, `stale`, `refused`, or `unconfirmed`.
    pub status: String,
    /// Refusal diagnostic, empty otherwise.
    pub message: String,
}

/// Project the runtime's decision classification without reinterpreting it.
#[must_use]
pub fn decision(event: &Event) -> Option<ApprovalDecision> {
    let Event::ApprovalDecision {
        id,
        decision,
        outcome,
    } = event
    else {
        return None;
    };
    let (status, message) = match outcome {
        DecisionOutcome::Decided => ("decided", String::new()),
        DecisionOutcome::Stale => ("stale", String::new()),
        DecisionOutcome::Refused { message, .. } => ("refused", message.clone()),
        DecisionOutcome::Unconfirmed => (
            "unconfirmed",
            "Re-read pending approvals before deciding again; the decision was not confirmed."
                .to_owned(),
        ),
    };
    Some(ApprovalDecision {
        id: id.to_string(),
        decision: decision.as_str().to_owned(),
        status: status.to_owned(),
        message,
    })
}

/// Versioned snapshot. Records contain exact references and identities, not values.
/// Hosts must label purpose and announced names as untrusted text, not identity.
///
/// # Errors
/// Returns the JSON encoder error if a record cannot be serialized.
pub fn pending_json(supported: bool, approvals: &[Approval]) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(
        &serde_json::json!({ "schema_version": 1, "supported": supported, "approvals": approvals }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::approvals::Decision;
    use phux_protocol::ids::ApprovalId;

    #[test]
    fn unconfirmed_decisions_are_not_projected_as_success() {
        let event = Event::ApprovalDecision {
            id: ApprovalId::parse(&"a".repeat(32)).unwrap(),
            decision: Decision::Approve,
            outcome: DecisionOutcome::Unconfirmed,
        };
        let projected = decision(&event).unwrap();
        assert_eq!(projected.status, "unconfirmed");
        assert_eq!(projected.decision, "approve");
        assert!(!projected.message.is_empty());
        assert!(decision(&Event::ApprovalsChanged).is_none());
    }
}
