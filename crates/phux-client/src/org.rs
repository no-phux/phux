//! Organization commands over one CLI or MCP connection (`docs/organization.md`),
//! behind `features_ext.ORGANIZATION`.
//!
//! Every helper here refuses before sending when the server does not
//! advertise the bit ([`OrgError::Unsupported`]); callers that keep a legacy
//! path branch on [`supported`] first. Every mutating command carries a
//! fresh [`OperationId`] unless the caller supplies one, and a connection
//! that drops after the command was sent reports
//! [`OrgError::OutcomeUnknown`] with that id, so the caller can resend the
//! same id on a new connection or reconcile from [`fetch`].
//!
//! Compare-and-swap edits (moves, relaunch, Intake resolve, arrangement
//! replace) re-read the records they name and retry when the server answers
//! `CONFLICT`, at most [`MAX_CAS_ATTEMPTS`] times. Removes, deletes,
//! renames, and lifetime changes carry no revision: one round trip each.
//!
//! These are the operations behind `phux rm`, `relaunch`, `restore`, `mv`,
//! bulk move, undo, and Intake resolve; the CLI and MCP adapters own
//! selector parsing and rendering.

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::OperationId;
use phux_protocol::wire::frame::{Command, Committed, ErrorCode, Refusal};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

pub use phux_client_core::org_ops::{OrgOutcome, is_inactive, live_target, refusal_sentence};
pub use phux_client_runtime::control::mint_operation;

mod edit;
mod inventory;
mod verbs;

pub use inventory::{KeyMatch, KeyNames, OrgInventory, arrangement, fetch, fetch_from_socket};
pub use verbs::{
    BatchResult, create_resource, delete_session, inbox_resolve, intake_resolve, move_many,
    move_resource, relaunch, remove_resource, rename_resource, replace_arrangement_of_member,
    replace_session_arrangement, restore, restore_session_on, session_project_supported,
    set_session_project, terminate, undo,
};

/// Most attempts a compare-and-swap edit makes before reporting the
/// conflict.
pub const MAX_CAS_ATTEMPTS: u32 = 4;

/// Records requested per `GET_ORGANIZATION` page.
const PAGE_SIZE: u32 = 512;

/// Most pages one [`fetch`] reads before giving up on a runaway cursor.
const MAX_PAGES: usize = 4096;

/// Why an organization request did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    /// The server does not advertise `ORGANIZATION`; nothing was sent.
    #[error("the server does not support the organization commands (it predates ORGANIZATION)")]
    Unsupported,
    /// The server refused the command and nothing changed.
    #[error("{}", refusal_sentence(.0))]
    Refused(Refusal),
    /// The server answered with a legacy `ERROR`.
    #[error("{message}")]
    Server {
        /// The wire error code.
        code: ErrorCode,
        /// The server's message.
        message: String,
    },
    /// The reply had a shape this command never produces.
    #[error("unexpected reply: {0}")]
    Unexpected(String),
    /// The connection dropped after the command was sent: it may or may not
    /// have committed. Resend `operation` on a new connection, or reconcile.
    #[error("the connection dropped before the server answered; the outcome is unknown")]
    OutcomeUnknown {
        /// The operation to resend or reconcile.
        operation: Option<OperationId>,
    },
    /// No record matches the target.
    #[error("no such record: {0}")]
    NotFound(String),
    /// A key prefix matches more than one record.
    #[error("key prefix {prefix:?} is ambiguous: {}", candidates.join(", "))]
    Ambiguous {
        /// The prefix as typed.
        prefix: String,
        /// The matching keys.
        candidates: Vec<String>,
    },
    /// Transport or decode failure before the command was sent.
    #[error(transparent)]
    Attach(AttachError),
}

impl OrgError {
    /// The refusal, when the server refused.
    #[must_use]
    pub const fn refusal(&self) -> Option<&Refusal> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            _ => None,
        }
    }

    /// Recovery facts preserved from this typed failure. Unknown identities stay absent.
    #[must_use]
    pub fn recovery(&self) -> crate::recovery::Recovery {
        use crate::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
        let operation = match self {
            Self::OutcomeUnknown { operation } => operation.map(OperationId::to_hex),
            _ => None,
        };
        let certainty = match self {
            Self::Unsupported | Self::Refused(_) | Self::NotFound(_) | Self::Ambiguous { .. } => {
                EffectCertainty::NotApplied
            }
            _ => EffectCertainty::Unknown,
        };
        Recovery::new(
            ScopeKind::OrganizationAuthority,
            operation,
            certainty,
            NextAction::Reconcile,
        )
    }

    /// A stable lowercase code for `--json` output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Refused(refusal) => refusal.as_str(),
            Self::Server { .. } => "server_error",
            Self::Unexpected(_) => "unexpected_reply",
            Self::OutcomeUnknown { .. } => "outcome_unknown",
            Self::NotFound(_) => "no_such_target",
            Self::Ambiguous { .. } => "selector_not_single",
            Self::Attach(_) => "transport",
        }
    }
}

/// Whether the server behind `conn` advertises `ORGANIZATION`.
#[must_use]
pub fn supported(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features_ext
            .contains(ServerFeatureExt::Organization)
    })
}

fn require(conn: &Connection) -> Result<(), OrgError> {
    if supported(conn) {
        Ok(())
    } else {
        Err(OrgError::Unsupported)
    }
}

/// Send one organization command and classify its reply.
///
/// # Errors
///
/// [`OrgError::Unsupported`] with nothing sent; [`OrgError::OutcomeUnknown`]
/// after any transport/decode failure following a possible mutating send.
/// Read-only requests retain their ordinary transport errors.
pub async fn request(
    conn: &mut Connection,
    request_id: u32,
    command: Command,
) -> Result<(OrgOutcome, Degradation), OrgError> {
    require(conn)?;
    let operation = command.idempotency_key().copied();
    let receipt_operation = operation.filter(|_| command.is_organization());
    match conn.request(request_id, command).await {
        Ok(reply) => {
            let (result, interleaved) = reply.into_parts();
            Ok((
                checked_outcome(&result, receipt_operation)?,
                Degradation::from_interleaved(&interleaved),
            ))
        }
        Err(_) if operation.is_some() => Err(OrgError::OutcomeUnknown { operation }),
        Err(AttachError::Disconnected) => Err(OrgError::OutcomeUnknown { operation }),
        Err(err) => Err(OrgError::Attach(err)),
    }
}

/// Send a mutating command and return its receipt.
///
/// # Errors
///
/// [`request`]'s errors, plus [`OrgError::Refused`] / [`OrgError::Server`]
/// for a refusal and [`OrgError::Unexpected`] for another reply shape.
pub async fn commit(
    conn: &mut Connection,
    request_id: u32,
    command: Command,
) -> Result<Committed, OrgError> {
    let operation = command.idempotency_key().copied();
    match request(conn, request_id, command).await?.0 {
        OrgOutcome::Committed(committed) => {
            if operation.is_some_and(|operation| operation != committed.operation) {
                return Err(OrgError::OutcomeUnknown { operation });
            }
            Ok(*committed)
        }
        OrgOutcome::Unknown { .. } | OrgOutcome::Accepted => {
            Err(OrgError::OutcomeUnknown { operation })
        }
        other => Err(outcome_error(other)),
    }
}

/// Send a command the server answers `ACCEPTED` (its effects follow as
/// events).
///
/// # Errors
///
/// As [`commit`].
pub async fn accept(
    conn: &mut Connection,
    request_id: u32,
    command: Command,
) -> Result<(), OrgError> {
    match request(conn, request_id, command).await?.0 {
        OrgOutcome::Accepted | OrgOutcome::Committed(_) => Ok(()),
        other => Err(outcome_error(other)),
    }
}

/// Keep ambiguous or foreign acknowledgements uncertain for every mutation caller,
/// including session creation paths that inspect the raw organization outcome.
fn checked_outcome(
    result: &phux_protocol::wire::frame::CommandResult,
    operation: Option<OperationId>,
) -> Result<OrgOutcome, OrgError> {
    let outcome = OrgOutcome::from_result(result);
    match &outcome {
        OrgOutcome::Committed(receipt)
            if operation.is_some_and(|expected| expected != receipt.operation) =>
        {
            Err(OrgError::OutcomeUnknown { operation })
        }
        OrgOutcome::Unknown { .. } | OrgOutcome::Value(_) if operation.is_some() => {
            Err(OrgError::OutcomeUnknown { operation })
        }
        _ => Ok(outcome),
    }
}

fn outcome_error(outcome: OrgOutcome) -> OrgError {
    match outcome {
        OrgOutcome::Refused(refusal) => OrgError::Refused(refusal),
        OrgOutcome::Error { code, message } => OrgError::Server { code, message },
        OrgOutcome::Unknown { reason } | OrgOutcome::NotSent { reason } => {
            OrgError::Unexpected(reason)
        }
        other => OrgError::Unexpected(format!("{other:?}")),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "recovery regression assertions"
)]
mod recovery_tests;
