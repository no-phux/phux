//! Validate and install persistent server state before the accept loops run.

use crate::runtime::config::{ServerConfig, ServerError, validate_socket_path_len};
use crate::runtime::connections::{
    handle_existing_socket, prepare_socket_dir, secure_socket_file, spawn_resource_exit_watcher,
};
use crate::runtime::{ServerEnv, resume, seed_session_with_actor, seed_session_with_pty, upgrade};
use crate::state::SharedState;
use crate::upgrade::blob::StateBlob;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;
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
        state.with_mut(|s| s.set_hub_table(table.clone()));
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
/// a fresh socket when this is a cold start.
pub(in crate::runtime) async fn adopt_or_bind_listener(
    socket_path: &Path,
    inherited_fd: Option<RawFd>,
) -> Result<crate::transport::UdsListener, ServerError> {
    if let Some(fd) = inherited_fd {
        let listener = resume::adopt_uds_listener(fd)?;
        info!(
            path = %socket_path.display(),
            "phux-server resumed; adopted the inherited UDS listener"
        );
        return Ok(listener);
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
    handle_existing_socket(socket_path).await?;
    let listener = UnixListener::bind(socket_path).map_err(ServerError::Bind)?;
    secure_socket_file(socket_path)?;
    let listener = crate::transport::UdsListener::new(listener);
    info!(path = %socket_path.display(), "phux-server listening on UDS");
    Ok(listener)
}

/// Mirror the configured defaults into shared state, so every pane spawn
/// site resolves them from one place.
pub(in crate::runtime) fn mirror_config_into_state(
    cfg: &ServerConfig,
    socket_path: &Path,
    state: &SharedState,
) {
    // A zero TTL would expire every hold before anyone could see it.
    let approval_ttl = Duration::from_secs(u64::from(cfg.approval_ttl_secs.max(1)));
    state
        .with(crate::state::ServerState::satellite_adopter)
        .set_work_store(cfg.work_store.as_deref().cloned());
    state.with_mut(|s| {
        s.set_pre_seeded_session(cfg.pre_seeded_session.clone());
        // Only the PTY mode is mirrored: a `CreateIfMissing` session runs its
        // own wire command, never the pre-seeded session's `seed_command`.
        s.set_attach_create_pty(cfg.seed_with_pty);
        // Injected as `PHUX_SOCKET` into every pane.
        s.set_server_socket_path(socket_path.to_path_buf());
        s.set_scrollback_limits(cfg.scrollback);
        s.set_agent_log_bytes(cfg.agent_log_bytes);
        s.set_native_host_executable(cfg.native_host_executable.clone());
        s.set_event_journal_bounds(
            usize::try_from(cfg.event_journal_entries).unwrap_or(usize::MAX),
            usize::try_from(cfg.event_journal_bytes).unwrap_or(usize::MAX),
        );
        s.set_retain_policy(cfg.retain);
        s.set_cwd_inheritance(cfg.cwd_inheritance);
        s.set_term(cfg.term.clone());
        s.set_shell(cfg.shell.clone());
        s.set_login_shell(cfg.login_shell);
        s.set_window_size(cfg.window_size);
        s.set_voice(cfg.voice.clone());
        s.set_metadata_value_bytes(cfg.metadata_value_bytes);
        s.set_approval_limits(
            approval_ttl,
            cfg.approval_max_pending,
            cfg.approval_max_pending_total,
        );
        if let Some(engine) = cfg.policy_engine.clone() {
            s.set_policy_engine(engine);
        }
        s.set_server_env(cfg.env.clone());
    });
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
        } else {
            let org = crate::state::fresh_org(instance, crate::state::now_stamp());
            s.install_org(store, org, false);
        }
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
    let rebuilt = state.with_mut(|s| {
        s.rebuild_from_blob(
            blob,
            crate::runtime::apps::terminal::upgrade::wired(state),
            crate::runtime::apps::rebuild_session,
        )
    })?;
    let mut pane_events: std::collections::HashMap<_, _> = rebuilt
        .wired_panes
        .into_iter()
        .map(|(pane, wiring)| {
            let wire = state.with_mut(|s| s.intern_resource_wire(pane));
            (pane, wiring.start(state, &wire))
        })
        .collect();
    for (resource, exit_notify) in rebuilt.exit_watchers {
        spawn_resource_exit_watcher(
            state.clone(),
            resource,
            Some(exit_notify),
            root_token.clone(),
            pane_events.remove(&resource),
        );
    }
    // A pane with no exit to watch (no PTY) still drains its events.
    for (pane, events) in pane_events {
        spawn_resource_exit_watcher(state.clone(), pane, None, root_token.clone(), Some(events));
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
    let seeded = if seed_with_pty {
        seed_session_with_pty(
            state,
            name,
            crate::runtime::apps::terminal::launch::seed_pane_command(state, seed_command),
            scrollback,
            root_token,
        )
    } else {
        seed_session_with_actor(state, name, scrollback, root_token)
    };
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
