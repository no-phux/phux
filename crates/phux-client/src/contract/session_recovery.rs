//! Preserve admitted/unknown session evidence through the existing error document.
use super::{Refusal, codes};
use crate::attach::AttachError;
use crate::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};

pub(super) fn session_refusal(error: &AttachError, verb: &str) -> Option<Refusal> {
    match error {
        AttachError::OperationOutcomeUnknown { operation } => {
            let unknown = crate::org::OrgError::OutcomeUnknown { operation: *operation };
            Some(Refusal::new(unknown.code(), format!("{verb}: {error}"),
                "reconcile the original session operation; do not submit a fresh create")
                .with_recovery(unknown.recovery()))
        }
        AttachError::CommittedSelectionUnavailable { operation, session, .. } => {
            Some(Refusal::new(codes::NO_SUCH_TARGET, format!("{verb}: {error}"),
                format!("session operation already committed; reconcile session {session} before selecting it, never create it again"))
                .with_recovery(Recovery::new(ScopeKind::OrganizationAuthority, Some(operation.to_hex()),
                    EffectCertainty::Applied, NextAction::Reconcile)))
        }
        _ => None,
    }
}
