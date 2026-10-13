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
//! (`workload-auth.md` §7); expiry ends them through the revocation
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

use std::io;
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use chrono::Utc;
use phux_core::ids::SessionId;
use phux_protocol::ids::{OperationId, SessionKey as WireSessionKey};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{CommandResult, CommandValue, Delegation, ErrorCode};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::input_lane::InputLaneHandle;
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::delegation::{DelegatedCredential, DelegationError, DelegationRequest};
use crate::policy::Revocation;
use crate::state::{ClientId, SharedState};
use crate::transport::{Incoming, UdsListener};
use crate::upgrade::blob::{DelegationBlob, StateBlob};

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

/// Handle `DELEGATE_AGENT` (`docs/spec/L1.md` §5.7.5) for `client_id`,
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

/// Handle `REVOKE_DELEGATION` (`docs/spec/L1.md` §5.7.5): idempotent, so a
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

/// The socket path for credential `id`: beside the server socket, named by
/// the first half of the credential's random id.
fn socket_path_for(server_socket: &Path, id: &str) -> PathBuf {
    let token = id.strip_prefix(crate::delegation::ID_PREFIX).unwrap_or(id);
    let short = token.get(..16).unwrap_or(token);
    let name = format!("{SOCKET_PREFIX}{short}{SOCKET_SUFFIX}");
    server_socket
        .parent()
        .map_or_else(|| PathBuf::from(&name), |dir| dir.join(&name))
}

/// A delegated socket's file name is `agent-<16 hex>.sock`.
const SOCKET_PREFIX: &str = "agent-";
const SOCKET_SUFFIX: &str = ".sock";

/// Whether `name` is spelled as a delegated socket's file name.
fn is_delegated_socket_name(name: &str) -> bool {
    name.strip_prefix(SOCKET_PREFIX)
        .and_then(|rest| rest.strip_suffix(SOCKET_SUFFIX))
        .is_some_and(|short| short.len() == 16 && short.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Add every live delegated credential and a duplicate of its listening
/// descriptor to `blob`, for the re-exec'd image to adopt. The duplicates
/// are the caller's to keep open until the exec.
///
/// # Errors
///
/// A descriptor that cannot be duplicated: the upgrade then aborts, as for
/// the server's own listener.
pub(super) fn carry_delegations(
    state: &SharedState,
    mut blob: StateBlob,
) -> io::Result<(StateBlob, Vec<OwnedFd>)> {
    let carried = state.with(|s| s.carried_delegations(Utc::now()));
    let mut owned = Vec::with_capacity(carried.len());
    blob.delegations = Vec::with_capacity(carried.len());
    for delegation in carried {
        // SAFETY: the listener task owns this descriptor and the state lock
        // was just released with the credential live; the borrow lasts only
        // for the dup.
        let duplicate =
            rustix::io::dup(unsafe { std::os::fd::BorrowedFd::borrow_raw(delegation.fd) })
                .map_err(io::Error::from)?;
        let credential = delegation.credential;
        blob.delegations.push(DelegationBlob {
            id: credential.id,
            group: credential.group,
            verbs: credential.verbs.bits(),
            on_behalf_of: credential.on_behalf_of,
            delegator: credential.delegator,
            expires_at_ms: credential.expires_at.timestamp_millis(),
            listener_fd: std::os::fd::AsRawFd::as_raw_fd(&duplicate),
            operations: delegation.operations,
        });
        owned.push(duplicate);
    }
    Ok((blob, owned))
}

/// Reinstate the delegated credentials a hot upgrade carried and serve
/// each one's inherited socket; returns how many. One that no longer
/// stands (expired, its session or launcher gone) is revoked: its
/// descriptor closes and its socket file is removed, so its agent's next
/// connection is refused. Every carried descriptor is owned here, so none
/// leaks.
pub(super) fn resume_delegations(
    state: &SharedState,
    carried: &[DelegationBlob],
    input_lane: Option<&InputLaneHandle>,
    root_token: &CancellationToken,
) -> usize {
    let server_socket = state.with(|s| s.server_socket_path().map(Path::to_path_buf));
    let mut pending: Vec<&DelegationBlob> = carried.iter().collect();
    let mut restored = 0;
    // A delegate's own delegator must be reinstated first: retry until a
    // pass reinstates nothing.
    loop {
        let before = pending.len();
        let mut unresolved = Vec::new();
        for blob in pending {
            match state.with_mut(|s| s.restore_delegation(blob, Utc::now())) {
                Ok(credential) => {
                    if serve_carried(state, blob, credential, input_lane, root_token) {
                        restored += 1;
                    }
                }
                Err(crate::state::UnrestoredDelegation::NoLauncher) => unresolved.push(blob),
                Err(reason) => drop_carried(server_socket.as_deref(), blob, reason),
            }
        }
        pending = unresolved;
        if pending.is_empty() || pending.len() == before {
            break;
        }
    }
    for blob in pending {
        drop_carried(
            server_socket.as_deref(),
            blob,
            crate::state::UnrestoredDelegation::NoLauncher,
        );
    }
    restored
}

/// Adopt a reinstated credential's inherited socket and serve it. `false`
/// (and the credential withdrawn) when the socket cannot be adopted.
fn serve_carried(
    state: &SharedState,
    blob: &DelegationBlob,
    credential: DelegatedCredential,
    input_lane: Option<&InputLaneHandle>,
    root_token: &CancellationToken,
) -> bool {
    let socket_path = state
        .with(|s| s.server_socket_path().map(Path::to_path_buf))
        .map(|server| socket_path_for(&server, &credential.id));
    let listener = super::resume::adopt_uds_listener(blob.listener_fd);
    let (Some(socket_path), Ok(listener)) = (socket_path, listener) else {
        warn!(credential = %credential.id, "could not adopt a carried delegated socket; revoked");
        state.with_mut(|s| s.withdraw_delegation(&credential.id));
        return false;
    };
    let close = root_token.child_token();
    let fd = listener.as_raw_fd();
    state.with_mut(|s| {
        s.attach_delegation_listener(&credential.id, socket_path.clone(), fd, close.clone())
    });
    info!(
        credential = %credential.id,
        expires_at = %credential.expires_at,
        "delegated agent credential carried across the upgrade"
    );
    spawn_listener(
        listener,
        socket_path,
        credential,
        state.clone(),
        close,
        input_lane.cloned(),
    );
    true
}

/// Revoke a carried credential that no longer stands: close its inherited
/// descriptor and remove its socket file.
fn drop_carried(
    server_socket: Option<&Path>,
    blob: &DelegationBlob,
    reason: crate::state::UnrestoredDelegation,
) {
    // SAFETY: the previous image handed this descriptor to this process
    // alone, for this credential; nothing else here holds it.
    drop(unsafe { OwnedFd::from_raw_fd(blob.listener_fd) });
    if crate::delegation::is_delegated_id(&blob.id)
        && let Some(server) = server_socket
    {
        remove_socket_file(&socket_path_for(server, &blob.id));
    }
    info!(credential = %blob.id, %reason, "carried delegated agent credential revoked");
}

/// Remove every delegated socket file beside the server's socket that no
/// live process serves: what a credential revoked by a cold restart, or not
/// carried by an upgrade, left behind. A carried credential's socket is
/// never probed, and one something else still accepts on (another server's)
/// is kept.
pub(super) fn sweep_stale_sockets(state: &SharedState) {
    let (server_socket, live) = state.with(|s| {
        (
            s.server_socket_path().map(Path::to_path_buf),
            s.carried_delegations(Utc::now())
                .into_iter()
                .map(|carried| carried.socket_path)
                .collect::<Vec<_>>(),
        )
    });
    let Some(dir) = server_socket.as_deref().and_then(Path::parent) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let named = entry
            .file_name()
            .to_str()
            .is_some_and(is_delegated_socket_name);
        let is_socket = entry
            .file_type()
            .is_ok_and(|kind| std::os::unix::fs::FileTypeExt::is_socket(&kind));
        if !named || !is_socket {
            continue;
        }
        let path = entry.path();
        if live.contains(&path) {
            continue;
        }
        if matches!(
            std::os::unix::net::UnixStream::connect(&path),
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused
        ) {
            debug!(path = %path.display(), "removed a stale delegated agent socket");
            remove_socket_file(&path);
        }
    }
}

/// The descriptor a carried delegation hands on, for clearing `FD_CLOEXEC`.
pub(super) const fn carried_fd(blob: &DelegationBlob) -> RawFd {
    blob.listener_fd
}

/// Bind and secure `credential`'s socket.
fn open_socket(
    state: &SharedState,
    credential: &DelegatedCredential,
) -> io::Result<(UdsListener, PathBuf)> {
    let server_socket = state
        .with(|s| s.server_socket_path().map(Path::to_path_buf))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the server has no socket"))?;
    let path = socket_path_for(&server_socket, &credential.id);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

    use std::os::fd::IntoRawFd as _;
    use std::time::{Duration, Instant};

    use tokio::io::AsyncWriteExt as _;
    use tokio::task::LocalSet;

    use super::*;
    use crate::delegation::DelegationRequest;
    use crate::policy::ConnectionGrant;
    use crate::runtime::test_support::{encode, hello};
    use crate::state::ClientId;

    const LAUNCHER: ClientId = ClientId(90);
    const PATIENCE: Duration = Duration::from_secs(5);

    /// A server with one session, `alpha`, its socket in `dir`, and the
    /// owner connected as [`LAUNCHER`].
    fn server(dir: &Path, root: &CancellationToken) -> (SharedState, SessionId) {
        let state = SharedState::new();
        let pane = super::super::commands::seed_session_with_actor(
            &state,
            "alpha",
            phux_config::ScrollbackLimits::default(),
            root,
        )
        .unwrap();
        let session = state.with_mut(|s| {
            s.set_server_socket_path(dir.join("phux.sock"));
            s.set_connection_grant(LAUNCHER, ConnectionGrant::owner());
            s.intern_upgrade_wires();
            s.registry().session_of(pane).unwrap()
        });
        (state, session)
    }

    /// The old image: mint a credential on `alpha` with its socket bound
    /// but not served, and carry it. The old image's own descriptor is
    /// closed, as the exec closes it; the carried duplicate keeps the
    /// socket bound.
    fn carried_by_old_image(dir: &Path) -> (DelegationBlob, PathBuf) {
        let root = CancellationToken::new();
        let (old, session) = server(dir, &root);
        let credential = old
            .with_mut(|s| {
                s.mint_delegation(LAUNCHER, session, DelegationRequest::default(), Utc::now())
            })
            .unwrap();
        let path = socket_path_for(&dir.join("phux.sock"), &credential.id);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&listener);
        old.with_mut(|s| s.note_delegation_operation([7; 16], &credential.id));
        assert!(old.with_mut(|s| {
            s.attach_delegation_listener(&credential.id, path.clone(), fd, CancellationToken::new())
        }));
        let empty =
            old.with(|s| s.assemble_upgrade_blob(0, &crate::state::UpgradeHandoffs::default()));
        let (blob, duplicates) = carry_delegations(&old, empty).unwrap();
        drop(listener);
        root.cancel();
        assert_eq!(blob.delegations.len(), 1);
        for duplicate in duplicates {
            // Handed to the new image, which owns it from here.
            let _ = duplicate.into_raw_fd();
        }
        (blob.delegations[0].clone(), path)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_hot_upgrade_carries_a_live_credential_and_its_socket() {
        LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let (carried, path) = carried_by_old_image(dir.path());

                let root = CancellationToken::new();
                let (new, _) = server(dir.path(), &root);
                assert_eq!(
                    resume_delegations(&new, std::slice::from_ref(&carried), None, &root),
                    1
                );
                let credential = new
                    .with(|s| s.delegation(&carried.id, Utc::now()).cloned())
                    .expect("reinstated");
                assert_eq!(credential.on_behalf_of, phux_core::PrincipalId::Owner);
                assert_eq!(
                    new.with(|s| {
                        s.delegation_minted_by(&[7; 16], Utc::now())
                            .map(|minted| minted.id.clone())
                    }),
                    Some(carried.id.clone()),
                    "a retry of the minting operation still answers it"
                );
                assert_eq!(
                    new.with(|s| s.delegation_socket(&carried.id).map(Path::to_path_buf)),
                    Some(path.clone())
                );

                // The agent reconnects through the same path, as itself.
                let mut stream = tokio::net::UnixStream::connect(&path).await.unwrap();
                stream
                    .write_all(&encode(&hello("delegated-agent")))
                    .await
                    .unwrap();
                let principal = credential.principal();
                let deadline = Instant::now() + PATIENCE;
                while !new.with(|s| {
                    s.watched_connections()
                        .iter()
                        .any(|watched| watched.grant.principal() == &principal)
                }) {
                    assert!(Instant::now() < deadline, "the agent never got its grant");
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }

                // The start-up sweep leaves a carried socket alone.
                sweep_stale_sockets(&new);
                assert!(path.exists());
                drop(stream);
                root.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_carried_credential_whose_session_is_gone_is_revoked_and_its_socket_removed() {
        LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let (mut carried, path) = carried_by_old_image(dir.path());
                carried.group += 100;

                let root = CancellationToken::new();
                let (new, _) = server(dir.path(), &root);
                assert_eq!(
                    resume_delegations(&new, std::slice::from_ref(&carried), None, &root),
                    0
                );
                assert!(new.with(|s| s.delegation(&carried.id, Utc::now()).is_none()));
                assert!(!path.exists(), "its socket file is removed");
                root.cancel();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_carried_credential_that_expired_meanwhile_is_revoked() {
        LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let (mut carried, path) = carried_by_old_image(dir.path());
                carried.expires_at_ms = Utc::now().timestamp_millis() - 1;

                let root = CancellationToken::new();
                let (new, _) = server(dir.path(), &root);
                assert_eq!(
                    resume_delegations(&new, std::slice::from_ref(&carried), None, &root),
                    0
                );
                assert!(!path.exists());
                root.cancel();
            })
            .await;
    }

    #[test]
    fn the_start_up_sweep_removes_only_unserved_delegated_sockets() {
        let dir = tempfile::tempdir().unwrap();
        let unserved = dir.path().join("agent-0123456789abcdef.sock");
        drop(std::os::unix::net::UnixListener::bind(&unserved).unwrap());
        let live_path = dir.path().join("agent-fedcba9876543210.sock");
        let live = std::os::unix::net::UnixListener::bind(&live_path).unwrap();
        let unrelated = dir.path().join("agent-notes.sock");
        drop(std::os::unix::net::UnixListener::bind(&unrelated).unwrap());

        let state = SharedState::new();
        state.with_mut(|s| s.set_server_socket_path(dir.path().join("phux.sock")));
        sweep_stale_sockets(&state);
        assert!(!unserved.exists(), "a socket nothing serves is removed");
        assert!(live_path.exists(), "a served socket is kept");
        assert!(unrelated.exists(), "only delegated socket names are swept");
        drop(live);
    }
}
