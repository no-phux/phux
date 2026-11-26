//! Validate and install persistent server state before the accept loops run.

use crate::runtime::config::{ServerError, validate_socket_path_len};
use crate::runtime::connections::{
    SocketLock, handle_existing_socket, prepare_socket_dir, secure_socket_file,
    spawn_resource_exit_watcher,
};
use crate::runtime::{ServerEnv, resume, upgrade};
use crate::state::SharedState;
use crate::upgrade::blob::StateBlob;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Validate the satellite registry before any I/O (hub mode only) and mirror
/// the table into state; the link supervisors are spawned from the result.
pub(in crate::runtime) fn install_hub_table(
    state: &SharedState,
    hub: bool,
    satellites: &[phux_config::SatelliteConfigEntry],
) -> Result<Option<crate::hub::HubTable>, ServerError> {
    let table = crate::hub::resolve_hub_table(hub, satellites)?;
    if let Some(table) = &table {
        info!(
            satellites = table.len(),
            "hub mode: satellite registry validated"
        );
        for (host, entry) in table.iter() {
            info!(satellite = %host, target = %entry.target, "hub satellite registered");
        }
        state.with_mut(|s| s.federation_mut().set_table(table.clone()));
    }
    Ok(table)
}

/// Load the consumer-token store the outbound connector supervisors
/// authenticate with. Token contents stay on disk and are re-read by each
/// supervisor attempt; only the store's presence is a startup gate.
pub(in crate::runtime) fn load_connector_consumer_tokens(
    specs: &[crate::connector::ConnectorSpec],
    env: &ServerEnv,
) -> Result<Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>, ServerError> {
    if specs.is_empty() {
        return Ok(None);
    }
    let path = env.tokens_path();
    if let Err(refusal) = phux_config::production::refuse_dev_on_production_state(&path) {
        return Err(ServerError::ConnectorTokenStore {
            path,
            source: crate::auth::AuthError::ProductionStore(refusal),
        });
    }
    let store = crate::auth::ReloadingTokenStore::load(path.clone()).map_err(|source| {
        ServerError::ConnectorTokenStore {
            path: path.clone(),
            source,
        }
    })?;
    Ok(Some(std::sync::Arc::new(store)))
}

/// Read the upgrade handoff blob (ADR-0032) and drop the previous image's
/// executable snapshot once the blob proves this is a resume.
pub(in crate::runtime) fn read_resume_blob(
    resume_fd: Option<RawFd>,
    inherited: &upgrade::InheritedUpgradeEnv,
) -> Result<Option<StateBlob>, ServerError> {
    let Some(fd) = resume_fd else {
        return Ok(None);
    };
    let blob = resume::read_blob_from_fd(fd)?;
    upgrade::cleanup_executable_snapshot(inherited.snapshot_dir.as_deref());
    Ok(Some(blob))
}

/// Adopt the listener inherited across a graceful upgrade (ADR-0032), or bind
/// a fresh socket when this is a cold start. Either way the server holds the
/// path's [`SocketLock`] (returned beside the listener) until it exits.
pub(in crate::runtime) async fn adopt_or_bind_listener(
    socket_path: &Path,
    inherited_fd: Option<RawFd>,
) -> Result<(crate::transport::UdsListener, Option<SocketLock>), ServerError> {
    if let Some(fd) = inherited_fd {
        let listener = resume::adopt_uds_listener(fd)?;
        info!(
            path = %socket_path.display(),
            "phux-server resumed; adopted the inherited UDS listener"
        );
        // The listener is already ours; refusing here would drop every pane.
        // Re-take the lock the previous image released at exec.
        let lock = SocketLock::acquire(socket_path)
            .inspect_err(|err| warn!(error = %err, "resumed without the socket lock"))
            .ok();
        return Ok((listener, lock));
    }
    validate_socket_path_len(socket_path)?;
    // Before anything that could unlink a stale production socket.
    phux_config::socket::refuse_dev_on_production(socket_path).map_err(|refusal| {
        ServerError::Bind(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            refusal,
        ))
    })?;
    prepare_socket_dir(socket_path)?;
    // The lock, not a connect probe, decides whether a server owns the path:
    // a busy live server can fail a probe, and unlinking its socket strands
    // its panes behind this one.
    let lock = SocketLock::acquire(socket_path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::WouldBlock {
            ServerError::SocketBusy(socket_path.to_path_buf())
        } else {
            // Creating the lock file is claiming the path: an unwritable
            // socket directory is a bind failure.
            ServerError::Bind(err)
        }
    })?;
    handle_existing_socket(socket_path).await?;
    let listener = UnixListener::bind(socket_path).map_err(ServerError::Bind)?;
    secure_socket_file(socket_path)?;
    let listener = crate::transport::UdsListener::new(listener);
    info!(path = %socket_path.display(), "phux-server listening on UDS");
    Ok((listener, Some(lock)))
}

/// Open the organization store and install its records; a store that was
/// never written leaves a fresh authority in memory for
/// [`crate::state::ServerState::persist_first_start`] to write once any
/// upgrade blob has been considered.
pub(in crate::runtime) fn open_org_store(
    state: &SharedState,
    location: &crate::OrgStoreLocation,
) -> Result<(), ServerError> {
    let mut store = location.open()?;
    let stored = store.load()?;
    state.with_mut(|s| -> Result<(), ServerError> {
        let instance = s.instance_id();
        if let Some(stored) = stored {
            let mut org = phux_core::Org::restore(
                stored.authority,
                instance,
                stored.sessions,
                stored.resources,
                stored.arrangements,
                stored.receipts,
            )?;
            // Every session has an arrangement; a store written before that
            // rule gets the missing ones now.
            if org.sessions().any(|session| session.arrangement.is_none()) {
                crate::state::ensure_arrangements(&mut org);
                store.import(&org, None, crate::state::now_stamp())?;
            }
            s.install_org(store, org, true);
            s.claim_unreadable_receipts(&stored.unreadable_receipts);
        } else {
            let org = crate::state::fresh_org(instance, crate::state::now_stamp());
            s.install_org(store, org, false);
        }
        crate::runtime::apps::terminal::program_resume::publish_program_resumes(s);
        Ok(())
    })?;
    if let Some(path) = location_path(location) {
        debug!(path = %path.display(), "organization store open");
    }
    Ok(())
}

pub(in crate::runtime) const fn location_path(
    location: &crate::OrgStoreLocation,
) -> Option<&PathBuf> {
    match location {
        crate::OrgStoreLocation::Path(path) => Some(path),
        crate::OrgStoreLocation::Memory => None,
    }
}

/// Rebuild the session tree from the upgrade blob (ADR-0032), all or
/// nothing, before any accept loop starts.
pub(in crate::runtime) fn resume_session_tree(
    state: &SharedState,
    blob: &StateBlob,
    root_token: &CancellationToken,
) -> Result<(), ServerError> {
    // Capture composition before taking the mutable registry lock. A rebuild
    // invokes the engine factory while that same lock remains held.
    let mut resumes = crate::runtime::upgrade::kinds::resumes(state);
    let rebuilt =
        state.with_mut(|s| crate::runtime::upgrade::kinds::rebuild(s, blob, &mut resumes))?;
    let mut pane_events: std::collections::HashMap<_, _> = resumes
        .iter_mut()
        .flat_map(|resume| resume.start(state))
        .collect();
    for (resource, activation, exit_notify) in rebuilt.exit_watchers {
        spawn_resource_exit_watcher(
            state.clone(),
            resource,
            activation,
            Some(exit_notify),
            root_token.clone(),
            pane_events.remove(&resource),
        );
    }
    // A pane with no exit to watch (no PTY) still drains its events.
    for (_, events) in pane_events {
        crate::runtime::connections::pane_events::spawn_pane_event_drain(state.clone(), events);
    }
    // ADR-0124 §6: retained exited panes close with `SERVER_SHUTDOWN`.
    state.with_mut(|s| s.close_upgrade_retained(blob));
    info!(
        sessions = blob.sessions.len(),
        panes = blob.panes.len(),
        agent_sessions = blob.agent_sessions.len(),
        "resumed session tree from upgrade blob"
    );
    Ok(())
}

/// A fresh start pre-seeds its single session instead of resuming one.
pub(in crate::runtime) fn seed_initial_session(
    state: &SharedState,
    name: &str,
    seed_with_pty: bool,
    seed_command: Option<crate::runtime::apps::terminal::launch::LaunchCommand>,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) {
    let seeded = crate::runtime::apps::terminal::launch::seed_initial_session(
        state,
        name,
        seed_with_pty,
        seed_command,
        scrollback,
        root_token,
    );
    if let Err(err) = seeded {
        warn!(
            session = name,
            error = %err,
            "failed to spawn pane actor for pre-seeded session",
        );
    } else {
        debug!(
            session = name,
            pty = seed_with_pty,
            "pre-seeded session in registry"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// While another server holds the path's lock, a cold start refuses even
    /// when the socket looks dead (a busy server's full backlog refuses a
    /// connect too) and leaves the entry alone. Once the holder is gone, the
    /// same stale entry is reaped and bound.
    #[tokio::test]
    async fn a_held_socket_lock_refuses_instead_of_unlinking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("phux.sock");
        // Bound then dropped: the file stays, nothing accepts on it.
        drop(std::os::unix::net::UnixListener::bind(&socket).expect("bind"));
        let holder = SocketLock::acquire(&socket).expect("first lock");

        let refused = adopt_or_bind_listener(&socket, None).await;
        assert!(
            matches!(refused, Err(ServerError::SocketBusy(_))),
            "{:?}",
            refused.err()
        );
        assert!(socket.exists(), "the lock holder's socket entry survives");

        drop(holder);
        let (_listener, lock) = adopt_or_bind_listener(&socket, None)
            .await
            .expect("an unheld stale socket is reaped");
        assert!(lock.is_some());
    }
}
