//! Delegated agent credentials at the transport (ADR-0163 §5).
//!
//! `HELLO` carries no credential, so a delegated credential is bound by
//! where a connection arrives: minting one opens an owner-only Unix socket
//! beside the server's own, and every connection accepted on it acts as that
//! credential, with its narrowed grant, its principal, and `on_behalf_of`
//! the launcher's human. An agent reaches it by running with `PHUX_SOCKET`
//! set to [`DelegatedAgent::socket_path`]; anything it spawns inherits that.
//!
//! The socket is a capability for attribution and least privilege among the
//! owner's own processes, not an isolation boundary: any process of the
//! serving uid can also reach the owner socket.
//!
//! **Lifetime.** The socket closes, and its file is removed, when the
//! credential is withdrawn ([`revoke_delegated_agent`]), when its session
//! is deleted, when it expires, or when the server stops. Withdrawal and
//! session deletion also revoke every live connection acting as it
//! (`docs/remote.md#managing-credentials`); expiry ends them through the revocation
//! watcher, which judges the grant's expiry.
//!
//! **Upgrade and restart.** A hot upgrade carries every live credential
//! and its bound socket (`carry_delegations`, `resume_delegations`):
//! the socket never goes unbound, so an agent's reconnect lands on the new
//! image as the same credential, provided its session survived and its
//! launcher still holds a credential. A cold restart carries nothing:
//! session wire ids restart, so every credential is revoked with the old
//! process, its socket file is swept at the next start
//! (`sweep_stale_sockets`), and the agent's connection is refused until
//! `phux agent start` mints a new one.

use std::path::PathBuf;

use chrono::Utc;
use phux_core::ids::SessionId;
use phux_protocol::ids::{OperationId, SessionKey as WireSessionKey};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{CommandResult, CommandValue, Delegation, ErrorCode};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::input_lane::InputLaneHandle;
use crate::auth::AuthenticatedCredential;
#[cfg(test)]
use crate::auth::ConnectionIdentity;
use crate::delegation::{DelegatedCredential, DelegationError, DelegationRequest};
use crate::policy::Revocation;
use crate::state::{ClientId, SharedState};

mod sockets;

#[cfg(doc)]
use sockets::DelegatedListener;
pub(super) use sockets::{carried_fd, carry_delegations, resume_delegations, sweep_stale_sockets};
use sockets::{open_socket, remove_socket_file, spawn_listener};

/// The `AuthenticatedCredential::principal` a delegated socket stamps on
/// every connection it accepts.
const DELEGATED_PRINCIPAL: &str = "delegated-agent";

/// A minted delegated credential and the socket its agent dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegatedAgent {
    /// The credential.
    pub credential: DelegatedCredential,
    /// The owner-only socket every connection of the agent must arrive on.
    pub socket_path: PathBuf,
}

/// Mint a delegated credential `launcher` hands to an agent on `session`,
/// and open the socket the agent connects through.
///
/// # Errors
///
/// The [`DelegationError`] from minting (the launcher would widen, lacks a
/// verb, or has no live grant; the session is gone), or
/// [`DelegationError::Socket`] when the socket cannot be opened. Nothing is
/// left minted on error.
pub fn delegate_agent(
    state: &SharedState,
    launcher: ClientId,
    session: SessionId,
    request: DelegationRequest,
    input_lane: Option<InputLaneHandle>,
    root_token: &CancellationToken,
) -> Result<DelegatedAgent, DelegationError> {
    let credential =
        state.with_mut(|s| s.mint_delegation(launcher, session, request, Utc::now()))?;
    let opened = open_socket(state, &credential);
    let (listener, socket_path) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            state.with_mut(|s| s.withdraw_delegation(&credential.id));
            return Err(DelegationError::Socket(error.to_string()));
        }
    };
    let close = root_token.child_token();
    let fd = listener.as_raw_fd();
    if !state.with_mut(|s| {
        s.attach_delegation_listener(&credential.id, socket_path.clone(), fd, close.clone())
    }) {
        remove_socket_file(&socket_path);
        return Err(DelegationError::NoLauncherGrant);
    }
    info!(
        credential = %credential.id,
        expires_at = %credential.expires_at,
        "delegated agent credential minted"
    );
    spawn_listener(
        listener,
        socket_path.clone(),
        credential.clone(),
        state.clone(),
        close,
        input_lane,
    );
    Ok(DelegatedAgent {
        credential,
        socket_path,
    })
}

/// Handle `DELEGATE_AGENT` (`docs/substrate.md`) for `client_id`,
/// which the dispatch guard admitted only on the owner socket. A repeat of
/// `operation` answers the live credential it minted.
pub(super) fn handle_delegate_agent(
    state: &SharedState,
    client_id: ClientId,
    operation: OperationId,
    session: &WireSessionKey,
    ttl_secs: u32,
    input_lane: Option<&InputLaneHandle>,
    root_token: &CancellationToken,
) -> CommandResult {
    let key = crate::org_wire::core_session_key(session);
    let prior = state.with(|s| {
        let credential = s.delegation_minted_by(operation.as_bytes(), Utc::now())?;
        let path = s.delegation_socket(&credential.id)?.to_path_buf();
        let same_session = s.registry().session_key(credential.session) == Some(key);
        Some((credential.clone(), path, same_session))
    });
    if let Some((credential, socket_path, same_session)) = prior {
        if !same_session {
            return error(
                ErrorCode::InvalidCommand,
                "DELEGATE_AGENT: this operation already minted a credential for another session",
            );
        }
        return delegation_value(
            *session,
            &DelegatedAgent {
                credential,
                socket_path,
            },
        );
    }
    let Some(core_session) = state.with(|s| s.registry().session_id(&key)) else {
        return error(
            ErrorCode::SessionNotFound,
            "DELEGATE_AGENT: no such session",
        );
    };
    let request = DelegationRequest {
        lifetime: if ttl_secs == 0 {
            crate::delegation::DEFAULT_LIFETIME
        } else {
            std::time::Duration::from_secs(u64::from(ttl_secs))
        },
        ..DelegationRequest::default()
    };
    match delegate_agent(
        state,
        client_id,
        core_session,
        request,
        input_lane.cloned(),
        root_token,
    ) {
        Ok(agent) => {
            state.with_mut(|s| {
                s.note_delegation_operation(*operation.as_bytes(), &agent.credential.id);
            });
            delegation_value(*session, &agent)
        }
        Err(refusal) => {
            let code = match refusal {
                DelegationError::NoSession => ErrorCode::SessionNotFound,
                DelegationError::NoLauncherGrant
                | DelegationError::Widens(_)
                | DelegationError::LauncherLacks(_) => ErrorCode::PermissionDenied,
                DelegationError::NoVerbs | DelegationError::Lifetime => ErrorCode::InvalidCommand,
                DelegationError::Random(_) | DelegationError::Socket(_) => ErrorCode::InternalError,
            };
            warn!(error = %refusal, "DELEGATE_AGENT refused");
            error(code, &format!("DELEGATE_AGENT: {refusal}"))
        }
    }
}

/// Handle `REVOKE_DELEGATION` (`docs/substrate.md`): idempotent, so a
/// credential already gone answers `Ok` too.
pub(super) fn handle_revoke_delegation(state: &SharedState, credential_id: &str) -> CommandResult {
    revoke_delegated_agent(state, credential_id);
    CommandResult::Ok
}

fn delegation_value(session: WireSessionKey, agent: &DelegatedAgent) -> CommandResult {
    CommandResult::OkWith(CommandValue::Delegation(Box::new(Delegation::new(
        agent.credential.id.clone(),
        session,
        agent.socket_path.to_string_lossy().into_owned(),
        u64::try_from(agent.credential.expires_at.timestamp_millis()).unwrap_or(0),
    ))))
}

fn error(code: ErrorCode, message: &str) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.to_owned(),
    }
}

/// Withdraw delegated credential `id`: close its socket and revoke every
/// live connection acting as it. `false` when `id` named nothing live.
pub fn revoke_delegated_agent(state: &SharedState, id: &str) -> bool {
    let live = state.with(|s| s.delegation(id, Utc::now()).is_some());
    let clients = state.with_mut(|s| s.withdraw_delegation(id));
    for client in clients {
        super::revocation::revoke_connection(state, client, Revocation::Revoked);
    }
    if live {
        info!(credential = %id, "delegated agent credential withdrawn");
    }
    live
}

/// The delegated credential a connection authenticated with, if it arrived
/// on a delegated socket: an owner-uid Unix connection stamped by
/// [`DelegatedListener`]. No other listener stamps that principal, and no
/// remote transport is a Unix socket.
pub(crate) fn delegated_credential<'a>(
    peer: &PeerIdentity,
    credential: Option<&'a AuthenticatedCredential>,
) -> Option<&'a str> {
    let credential = credential?;
    (matches!(peer.transport, TransportType::UnixSocket)
        && peer.uid == rustix::process::geteuid().as_raw()
        && credential.principal == DELEGATED_PRINCIPAL
        && crate::delegation::is_delegated_id(&credential.id))
    .then_some(credential.id.as_str())
}

/// The attestation a delegated socket stamps on each connection it
/// accepts: `credential`'s id, scopes, and expiry.
fn stamp(credential: &DelegatedCredential) -> AuthenticatedCredential {
    AuthenticatedCredential {
        id: credential.id.clone(),
        principal: DELEGATED_PRINCIPAL.to_owned(),
        scopes: credential
            .scopes()
            .grants()
            .iter()
            .map(ToString::to_string)
            .collect(),
        issued_at: Utc::now(),
        expires_at: Some(credential.expires_at),
        generation: 0,
        registry_instance: None,
    }
}

/// The identity a connection from `peer` has when it arrives on
/// `credential`'s socket.
#[cfg(test)]
pub(super) fn stamped_identity(
    peer: PeerIdentity,
    credential: &DelegatedCredential,
) -> ConnectionIdentity {
    let mut identity = ConnectionIdentity::from(peer);
    identity.credential = Some(stamp(credential));
    identity
}

#[cfg(test)]
mod tests;
