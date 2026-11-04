//! The delegated sockets: their names, their carry across a hot upgrade,
//! and the listener behind each one.

use std::io;
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use chrono::Utc;
use phux_protocol::policy::TransportType;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::stamp;
use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::delegation::DelegatedCredential;
use crate::runtime::apps::terminal::input_lane::InputLaneHandle;
use crate::state::SharedState;
use crate::transport::{Incoming, UdsListener};
use crate::upgrade::blob::{DelegationBlob, StateBlob};

/// The socket path for credential `id`: beside the server socket, named by
/// the first half of the credential's random id.
pub(super) fn socket_path_for(server_socket: &Path, id: &str) -> PathBuf {
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
pub(in crate::runtime) fn carry_delegations(
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
pub(in crate::runtime) fn resume_delegations(
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
    let listener = super::super::resume::adopt_uds_listener(blob.listener_fd);
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
pub(in crate::runtime) fn sweep_stale_sockets(state: &SharedState) {
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
pub(in crate::runtime) const fn carried_fd(blob: &DelegationBlob) -> RawFd {
    blob.listener_fd
}

/// Bind and secure `credential`'s socket.
pub(super) fn open_socket(
    state: &SharedState,
    credential: &DelegatedCredential,
) -> io::Result<(UdsListener, PathBuf)> {
    let server_socket = state
        .with(|s| s.server_socket_path().map(Path::to_path_buf))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the server has no socket"))?;
    let path = socket_path_for(&server_socket, &credential.id);
    super::super::validate_socket_path_len(&path)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let listener = tokio::net::UnixListener::bind(&path)?;
    if let Err(error) = crate::runtime::connections::secure_socket_file(&path) {
        remove_socket_file(&path);
        return Err(io::Error::other(error.to_string()));
    }
    Ok((UdsListener::new(listener), path))
}

/// Run `credential`'s accept loop until it is withdrawn, it expires, or the
/// server stops; then remove its socket file and forget it.
pub(super) fn spawn_listener(
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
        let accept = crate::runtime::connections::accept_loop(
            &listener,
            state.clone(),
            close.clone(),
            input_lane,
        );
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

pub(super) fn remove_socket_file(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        warn!(%error, "could not remove a delegated agent socket");
    }
}

/// A Unix listener whose every connection carries one delegated
/// credential.
pub(super) struct DelegatedListener {
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
