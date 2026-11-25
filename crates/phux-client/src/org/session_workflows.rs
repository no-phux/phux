//! Native projections of exact create/ensure and keyed post-commit attachment.
use phux_protocol::ids::{OperationId, SessionKey};
use phux_protocol::wire::frame::{
    AttachTarget, Command, Committed, LaunchSpec, OrgFilter, SessionLifetime,
};

use super::{OrgError, OrgInventory, commit, fetch};
use crate::attach::connection::Connection;

/// Create exactly one named session; a collision is a host refusal, never ensure.
/// The caller retains `operation` after any ambiguous transport outcome.
///
/// # Errors
/// Unsupported peers accept no work. All other errors preserve `commit`'s
/// refusal/unknown distinction; callers must not replace an unknown operation.
pub async fn create_session(
    conn: &mut Connection,
    operation: OperationId,
    name: String,
    lifetime: SessionLifetime,
    launch: Option<Box<LaunchSpec>>,
) -> Result<Committed, OrgError> {
    commit(
        conn,
        super::edit::FIRST_REQUEST_ID,
        Command::SessionCreate {
            operation,
            name,
            lifetime,
            attrs: Vec::new(),
            launch,
        },
    )
    .await
}

/// Ask the host to atomically resolve existing versus created under one operation.
/// Existing sessions ignore the seed launch and retain their execution unchanged.
///
/// # Errors
/// Older peers are unsupported, with no local exists/create or legacy attach
/// fallback. Missing receipt identity/outcome remains unknown under `operation`.
pub async fn ensure_session(
    conn: &mut Connection,
    operation: OperationId,
    name: String,
    lifetime: SessionLifetime,
    launch: Option<Box<LaunchSpec>>,
    attrs: Vec<(String, String)>,
) -> Result<Committed, OrgError> {
    commit(
        conn,
        super::edit::FIRST_REQUEST_ID,
        Command::SessionEnsure {
            operation,
            name,
            lifetime,
            launch,
            attrs,
        },
    )
    .await
}

/// Resolve only the receipt's exact durable key to its current attach handle.
/// A removed/unavailable key never falls back to a possibly reused name.
///
/// # Errors
/// Missing receipt identity is unknown; unavailable live selection is `NotFound`.
pub async fn attach_target_for_receipt(
    conn: &mut Connection,
    receipt: &Committed,
) -> Result<AttachTarget, OrgError> {
    let (key, inventory) = session_inventory_for_receipt(conn, receipt).await?;
    inventory
        .session(&key)
        .and_then(|session| session.live)
        .map(AttachTarget::ById)
        .ok_or_else(|| OrgError::NotFound(key.to_string()))
}

/// Read the exact session cut named by a receipt, retaining its authority fence.
/// Both attach and seed presentation use this without a second existence policy.
///
/// # Errors
/// Missing receipt identity is unknown; a changed authority is unavailable.
pub async fn session_inventory_for_receipt(
    conn: &mut Connection,
    receipt: &Committed,
) -> Result<(SessionKey, OrgInventory), OrgError> {
    let key = receipt.session.ok_or(OrgError::OutcomeUnknown {
        operation: Some(receipt.operation),
    })?;
    let inventory = fetch(conn, OrgFilter::Session(key), super::edit::FIRST_REQUEST_ID).await?;
    if receipt
        .authority
        .is_some_and(|authority| authority != inventory.authority)
    {
        return Err(OrgError::NotFound(format!(
            "session {key} belongs to another authority"
        )));
    }
    Ok((key, inventory))
}

#[cfg(test)]
mod tests;
