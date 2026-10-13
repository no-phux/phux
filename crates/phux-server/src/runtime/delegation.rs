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
//! credential is withdrawn ([`revoke_delegated_agent`]), when it expires,
//! or when the server stops. Withdrawal also revokes every live connection
//! acting as it (`workload-auth.md` §7); expiry ends them through the
//! revocation watcher, which judges the grant's expiry.

use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use phux_core::ids::SessionId;
use phux_protocol::policy::{PeerIdentity, TransportType};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::input_lane::InputLaneHandle;
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::delegation::{DelegatedCredential, DelegationError, DelegationRequest};
use crate::policy::Revocation;
use crate::state::{ClientId, SharedState};
use crate::transport::{Incoming, UdsListener};

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
    if !state.with_mut(|s| s.attach_delegation_listener(&credential.id, close.clone())) {
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

/// The socket path for `credential`: beside the server socket, named by
/// the first half of the credential's random id.
fn socket_path_for(server_socket: &Path, credential: &DelegatedCredential) -> PathBuf {
    let token = credential
        .id
        .strip_prefix(crate::delegation::ID_PREFIX)
        .unwrap_or(&credential.id);
    let short = token.get(..16).unwrap_or(token);
    let name = format!("agent-{short}.sock");
    server_socket
        .parent()
        .map_or_else(|| PathBuf::from(&name), |dir| dir.join(&name))
}

/// Bind and secure `credential`'s socket.
fn open_socket(
    state: &SharedState,
    credential: &DelegatedCredential,
) -> io::Result<(UdsListener, PathBuf)> {
    let server_socket = state
        .with(|s| s.server_socket_path().map(Path::to_path_buf))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the server has no socket"))?;
    let path = socket_path_for(&server_socket, credential);
    super::validate_socket_path_len(&path).map_err(|error| io::Error::other(error.to_string()))?;
    let listener = tokio::net::UnixListener::bind(&path)?;
    if let Err(error) = super::client::secure_socket_file(&path) {
        remove_socket_file(&path);
        return Err(io::Error::other(error.to_string()));
    }
    Ok((UdsListener::new(listener), path))
}

/// Run `credential`'s accept loop until it is withdrawn, it expires, or the
/// server stops; then remove its socket file and forget it.
fn spawn_listener(
    listener: UdsListener,
    socket_path: PathBuf,
    credential: DelegatedCredential,
    state: SharedState,
    close: CancellationToken,
    input_lane: Option<InputLaneHandle>,
) {
    let listener = DelegatedListener {
        inner: listener,
        credential: stamp(&credential),
    };
    let lifetime = (credential.expires_at - Utc::now())
        .to_std()
        .unwrap_or_default();
    tokio::task::spawn_local(async move {
        let expiry = tokio::time::sleep(lifetime);
        let accept =
            super::client::accept_loop(&listener, state.clone(), close.clone(), input_lane);
        tokio::select! {
            result = accept => {
                if let Err(error) = result {
                    warn!(credential = %credential.id, %error, "delegated socket accept loop ended");
                }
            }
            () = expiry => {
                debug!(credential = %credential.id, "delegated agent credential expired");
            }
        }
        state.with_mut(|s| s.withdraw_delegation(&credential.id));
        close.cancel();
        remove_socket_file(&socket_path);
    });
}

fn remove_socket_file(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        warn!(%error, "could not remove a delegated agent socket");
    }
}

/// A Unix listener whose every connection carries one delegated
/// credential.
struct DelegatedListener {
    inner: UdsListener,
    credential: AuthenticatedCredential,
}

impl Incoming for DelegatedListener {
    type Reader = <UdsListener as Incoming>::Reader;
    type Writer = <UdsListener as Incoming>::Writer;

    async fn accept(&self) -> io::Result<(Self::Reader, Self::Writer, ConnectionIdentity)> {
        let (reader, writer, mut identity) = self.inner.accept().await?;
        identity.credential = Some(self.credential.clone());
        Ok((reader, writer, identity))
    }

    fn transport_type(&self) -> TransportType {
        TransportType::UnixSocket
    }

    fn kind(&self) -> &'static str {
        "agent-uds"
    }
}
