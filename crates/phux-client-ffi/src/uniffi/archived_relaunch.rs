//! Local-owner archived recipe review. RemoteClient cannot impersonate it.
use std::sync::{Arc, Mutex};
use phux_client_runtime::{Target, archived_relaunch::RelaunchReview};
use phux_protocol::ids::{ResourceKey, Revision};
use phux_protocol::wire::frame::{CommandResult, CommandValue};

/// A failed inert preparation; no execution was admitted.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RawRelaunchError {
    /// Invalid target, unsupported peer, or host refusal.
    #[error("{reason}")]
    Refused { reason: String },
}

/// Admission and uncertainty are not evidence that execution started.
#[derive(Debug, uniffi::Enum)]
pub enum RawRelaunchOutcome {
    /// The exact operation was admitted; observe execution separately.
    Accepted,
    /// The exact operation committed; observe execution separately.
    Committed,
    /// Host refused before admission.
    Refused { reason: String },
    /// Delivery may have occurred; reconcile the displayed operation.
    Unknown { reason: String },
}

/// Whether review cancellation actually prevented submission.
#[derive(Debug, uniffi::Enum)]
pub enum RawRelaunchCancellation {
    /// The retained connection was closed before any approval was submitted.
    CancelledBeforeSubmission,
    /// Already approved or cancelled; this call cannot assert no execution.
    AlreadyConsumed,
    /// The retained state could not be inspected; no cancellation is promised.
    Unavailable,
}

/// One proposal and one live local-owner connection, never reconnectable.
/// Foreign UIs must show the entire document and pass that exact document only
/// after a distinct explicit owner action. Dropping an unsubmitted review cancels it.
#[derive(uniffi::Object)]
pub struct RawRelaunchReview {
    document: String,
    inner: Mutex<Option<RelaunchReview>>,
}

#[uniffi::export]
impl RawRelaunchReview {
    /// Call on a worker. This constructor only reads an inert proposal.
    /// A phone/remote client must not substitute its own local socket for a
    /// remote executing host. Use the exact local desktop host socket only.
    #[uniffi::constructor]
    pub fn prepare(socket: String, resource: String, revision: u64) -> Result<Arc<Self>, RawRelaunchError> {
        let fail = |reason| RawRelaunchError::Refused { reason };
        let key = ResourceKey::parse(&resource).ok_or_else(|| fail("invalid_resource_key".into()))?;
        let review = RelaunchReview::prepare(Target::uds(socket.into()),
            phux_client_runtime::control::mint_operation(), key, Revision::new(revision)).map_err(fail)?;
        let document = review.document().to_owned();
        Ok(Arc::new(Self { document, inner: Mutex::new(Some(review)) }))
    }

    /// Complete, escaped executable input, target, revision and operation.
    /// Render it in a scrollable, selectable review surface, never a toast.
    pub fn document(&self) -> String { self.document.clone() }

    /// Explicit owner action. Echo the displayed exact document, not a bool.
    /// Consumes this proposal; a second invocation cannot execute again.
    pub fn approve(&self, reviewed_document: String) -> RawRelaunchOutcome {
        if reviewed_document != self.document {
            return RawRelaunchOutcome::Refused { reason: "reviewed document changed".into() };
        }
        let review = match self.inner.lock() {
            Ok(mut inner) => inner.take(),
            Err(_) => return RawRelaunchOutcome::Refused { reason: "review state unavailable".into() },
        };
        let Some(review) = review else {
            return RawRelaunchOutcome::Refused { reason: "review was already consumed or cancelled".into() };
        };
        match review.approve(&reviewed_document) {
            Ok(CommandResult::Accepted { .. }) => RawRelaunchOutcome::Accepted,
            Ok(CommandResult::OkWith(CommandValue::Committed(_))) => RawRelaunchOutcome::Committed,
            Ok(CommandResult::Refused(reason)) => RawRelaunchOutcome::Refused { reason: format!("{reason:?}") },
            Ok(CommandResult::Error { code: phux_protocol::wire::frame::ErrorCode::Unsupported | phux_protocol::wire::frame::ErrorCode::PermissionDenied, message }) => RawRelaunchOutcome::Refused { reason: message },
            Ok(CommandResult::Error { message, .. }) => RawRelaunchOutcome::Unknown { reason: message },
            Ok(other) => RawRelaunchOutcome::Unknown { reason: format!("unrecognized admission reply: {other:?}") },
            Err(reason) => RawRelaunchOutcome::Unknown { reason },
        }
    }
    /// Cancel only a still-pending review. This cannot retract a submitted operation.
    pub fn cancel(&self) -> RawRelaunchCancellation {
        match self.inner.lock() {
            Ok(mut inner) => if inner.take().is_some() {
                RawRelaunchCancellation::CancelledBeforeSubmission
            } else {
                RawRelaunchCancellation::AlreadyConsumed
            },
            Err(_) => RawRelaunchCancellation::Unavailable,
        }
    }
}
