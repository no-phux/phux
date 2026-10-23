//! Organization commands over one CLI or MCP connection (`docs/spec/L1.md`
//! §5.7), behind `features_ext.ORGANIZATION`.
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

use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{
    AuthorityId, KEY_MIN_PREFIX, OperationId, ResourceKey, ResourceKind, Revision, SessionId,
    SessionKey,
};
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementValue, Command, CommandValue, Committed, ErrorCode, ExecState,
    ExitPolicy, LaunchMode, LaunchSpec, MAX_ORG_BATCH, MoveItem, OrgFilter, OrganizationPage,
    Outcome, Refusal, ResourceRecord, SessionRecord, TerminateMode,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

pub use phux_client_core::org_ops::{OrgOutcome, is_inactive, live_target, refusal_sentence};
pub use phux_client_runtime::control::mint_operation;

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
    /// No trustworthy outcome was received after a possible send. It may
    /// have committed; reconcile before considering a retained identical retry.
    #[error("the operation outcome could not be confirmed; reconcile before retrying")]
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

    /// A stable lowercase code for `--json` output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.api_code().as_str()
    }

    /// Typed code used by public workflow error DTOs and their schemas.
    #[must_use]
    pub const fn api_code(&self) -> phux_protocol::api::ErrorCode {
        use phux_protocol::api::ErrorCode as C;
        match self {
            Self::Unsupported => C::Unsupported,
            Self::Refused(refusal) => C::from_refusal(refusal),
            Self::Server { .. } => C::ServerError,
            Self::Unexpected(_) => C::UnexpectedReply,
            Self::OutcomeUnknown { .. } => C::OutcomeUnknown,
            Self::NotFound(_) => C::NoSuchTarget,
            Self::Ambiguous { .. } => C::SelectorNotSingle,
            Self::Attach(_) => C::Transport,
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
/// when the connection drops before the reply; [`OrgError::Attach`] for any
/// other transport failure.
pub async fn request(
    conn: &mut Connection,
    request_id: u32,
    command: Command,
) -> Result<(OrgOutcome, Degradation), OrgError> {
    require(conn)?;
    if additive_feature(&command)
        .is_some_and(|feature| !conn.server_features_ext().contains(feature))
    {
        return Err(OrgError::Unsupported);
    }
    let operation = command.idempotency_key().copied();
    match conn.request(request_id, command).await {
        Ok(reply) => {
            let (result, interleaved) = reply.into_parts();
            Ok((
                OrgOutcome::from_result(&result),
                Degradation::from_interleaved(&interleaved),
            ))
        }
        Err(_) if operation.is_some() => Err(OrgError::OutcomeUnknown { operation }),
        Err(AttachError::Disconnected) => Err(OrgError::OutcomeUnknown { operation }),
        Err(err) => Err(OrgError::Attach(err)),
    }
}

const fn additive_feature(command: &Command) -> Option<ServerFeatureExt> {
    match command {
        Command::ResourceCreate {
            placement: Some(_), ..
        } => Some(ServerFeatureExt::CreatePlacement),
        Command::GetOrganization {
            filter: OrgFilter::Resource(_),
            ..
        } => Some(ServerFeatureExt::DurableResourceInspection),
        _ => None,
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
        OrgOutcome::Committed(committed) => Ok(*committed),
        OrgOutcome::Unknown { .. } => Err(OrgError::OutcomeUnknown { operation }),
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

mod inventory;
pub use inventory::*;
mod edits;
pub use edits::*;
