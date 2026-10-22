//! Delegated agent credentials over one owner connection
//! (`docs/substrate.md#narrow-delegation-that-keeps-the-human`, ADR-0163).
//!
//! [`delegate_agent`] asks the server to mint a credential an agent acts
//! through: `INVENTORY`, `OBSERVE`, `INPUT`, and `BIND` on one session, on behalf of
//! the caller, bound to an owner-only socket of its own. The agent then runs
//! with `PHUX_SOCKET` set to [`Delegation::socket_path`], so every phux
//! command it issues is attributed to the credential and confined to its
//! grant. [`revoke_delegation`] withdraws it. Both commands are owner-only:
//! the server refuses them off the owner's Unix socket.

use phux_protocol::ids::{OperationId, SessionKey};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};

pub use phux_protocol::wire::frame::Delegation;

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Why a delegation request did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    /// The server answered with an error: refused (`PERMISSION_DENIED` off
    /// the owner socket, or a request that would widen a grant), an unknown
    /// session, or a server that predates the command.
    #[error("{message}")]
    Server {
        /// The wire error code.
        code: ErrorCode,
        /// The server's message.
        message: String,
    },
    /// The reply had a shape this command never produces.
    #[error("unexpected reply: {0:?}")]
    Unexpected(Box<CommandResult>),
    /// The connection dropped after the command was sent. A
    /// `DELEGATE_AGENT` may have minted: resend `operation` on a new
    /// connection to get the same credential.
    #[error("the connection dropped before the server answered; the outcome is unknown")]
    OutcomeUnknown {
        /// The operation to resend.
        operation: OperationId,
    },
    /// Any other transport failure.
    #[error(transparent)]
    Attach(AttachError),
}

/// Mint a delegated agent credential on `session` for `ttl_secs` seconds
/// (`0` = the server default), under `operation` (a retry with the same id
/// answers the same credential).
///
/// # Errors
///
/// A [`DelegationError`]; nothing was minted unless it is
/// [`DelegationError::OutcomeUnknown`].
pub async fn delegate_agent(
    conn: &mut Connection,
    request_id: u32,
    operation: OperationId,
    session: SessionKey,
    ttl_secs: u32,
) -> Result<Delegation, DelegationError> {
    let command = Command::DelegateAgent {
        operation,
        session,
        ttl_secs,
    };
    match send(conn, request_id, operation, command).await? {
        CommandResult::OkWith(CommandValue::Delegation(delegation)) => Ok(*delegation),
        other => Err(unexpected(other)),
    }
}

/// Withdraw delegated credential `credential_id`: its socket closes and its
/// live connections are revoked. Succeeds when it is already gone.
///
/// # Errors
///
/// A [`DelegationError`].
pub async fn revoke_delegation(
    conn: &mut Connection,
    request_id: u32,
    operation: OperationId,
    credential_id: &str,
) -> Result<(), DelegationError> {
    let command = Command::RevokeDelegation {
        operation,
        credential_id: credential_id.to_owned(),
    };
    match send(conn, request_id, operation, command).await? {
        CommandResult::Ok => Ok(()),
        other => Err(unexpected(other)),
    }
}

async fn send(
    conn: &mut Connection,
    request_id: u32,
    operation: OperationId,
    command: Command,
) -> Result<CommandResult, DelegationError> {
    match conn.request(request_id, command).await {
        Ok(reply) => Ok(reply.into_parts().0),
        Err(AttachError::Disconnected) => Err(DelegationError::OutcomeUnknown { operation }),
        Err(err) => Err(DelegationError::Attach(err)),
    }
}

fn unexpected(result: CommandResult) -> DelegationError {
    match result {
        CommandResult::Error { code, message } => DelegationError::Server { code, message },
        other => DelegationError::Unexpected(Box::new(other)),
    }
}
