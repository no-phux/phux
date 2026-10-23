//! Inventory: extracted from the shared module.
use super::{
    CancellationToken, ClientId, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
    Outbound, SeedOrigin, SharedState, StateScope, debug, empty_session_snapshot, seed_session,
    warn,
};

/// A PTY command from a non-empty request argv.
pub(crate) fn argv_command(argv: Option<Vec<String>>) -> Option<portable_pty::CommandBuilder> {
    let mut argv = argv?.into_iter();
    let mut builder = portable_pty::CommandBuilder::new(argv.next()?);
    for arg in argv {
        builder.arg(arg);
    }
    Some(builder)
}

/// Create a named session and seed its pane without attaching: the
/// `SESSION_CREATE_KEY` metadata write (ADR-0019 / ADR-0027).
///
/// The existence check and seed run on the single-threaded runtime, so two
/// racing creates for one `name` cannot both succeed. Returns the seed pane's
/// wire id; `Err` is log-only because `SET_METADATA` has no reply frame.
pub(crate) fn create_named_session(
    state: &SharedState,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<&str>,
    env: std::collections::BTreeMap<String, String>,
    origin: SeedOrigin,
    root_token: &CancellationToken,
) -> Result<phux_protocol::ids::ResourceId, String> {
    if state.with(|s| s.session_by_name(name).is_some()) {
        return Err(format!("session {name:?} already exists"));
    }

    let (with_pty, override_cmd, scrollback, term, shell, login_shell) = state.with(|s| {
        (
            s.attach_create_seeds_pty(),
            s.attach_create_seed_command(),
            s.scrollback_limits(),
            s.term().to_owned(),
            s.shell().to_owned(),
            s.login_shell(),
        )
    });

    let seed_cmd = with_pty.then(|| {
        // A server-wide override wins, then the request argv, then the shell.
        let mut seed_cmd = override_cmd.unwrap_or_else(|| {
            argv_command(command).unwrap_or_else(|| {
                crate::terminal_actor::default_shell_command(&shell, login_shell)
            })
        });
        // An invalid cwd is dropped with a warn, never failing the create.
        crate::terminal_actor::apply_spawn_cwd(&mut seed_cmd, cwd, name);
        for (key, value) in env {
            seed_cmd.env(key, value);
        }
        crate::terminal_actor::apply_term(&mut seed_cmd, &term);
        seed_cmd
    });
    match seed_session(state, name, seed_cmd, scrollback, root_token, None, origin) {
        Ok(core_terminal) => {
            // A headless create arms the last-session self-exit like an attach.
            let wire = state.with_mut(|s| {
                s.arm_self_exit();
                s.intern_terminal_wire(core_terminal)
            });
            Ok(wire)
        }
        Err(err) => {
            warn!(
                session = %name,
                error = %err,
                "session-create: failed to seed pane for new session",
            );
            Err(format!("failed to create session {name:?}: {err}"))
        }
    }
}

/// Commit an empty persistent session, the `empty: true` form of the
/// `SESSION_CREATE_KEY` write, attributed to `actor`. It does not arm
/// self-exit: an empty session runs nothing, so the writer's disconnect
/// must not end the server it just created a session on.
pub(crate) fn create_empty_session(
    state: &SharedState,
    name: &str,
    actor: &phux_core::Actor,
) -> Result<(), String> {
    state.with_mut(|s| {
        if s.session_by_name(name).is_some() {
            return Err(format!("session {name:?} already exists"));
        }
        s.create_empty_session(name, phux_core::SessionLifetime::Persistent, actor)
            .map(|_| ())
            .map_err(|refusal| format!("failed to create session {name:?}: {refusal}"))
    })
}

/// `GET_PERF`: the server's telemetry as a JSON `phux_perf::PerfReport`,
/// with registry gauges refreshed first; `reset` zeroes metrics afterwards.
pub(crate) fn handle_get_perf(state: &SharedState, reset: bool) -> CommandResult {
    let (sessions, panes) =
        state.with_mut(|s| (s.registry().session_count(), s.registry().terminal_count()));
    let clients = match handle_get_state(state, None, &StateScope::Server) {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot
            .sessions
            .iter()
            .map(|s| u64::from(s.attached_client_count))
            .sum::<u64>(),
        _ => 0,
    };
    crate::perf::SESSIONS.set(u64::try_from(sessions).unwrap_or(u64::MAX));
    crate::perf::PANES.set(u64::try_from(panes).unwrap_or(u64::MAX));
    crate::perf::CLIENTS.set(clients);
    let mut report = crate::perf::report();
    report.stream_diagnostics = serde_json::to_value(crate::stream_diagnostics::snapshot()).ok();
    if reset {
        crate::perf::reset();
        crate::stream_diagnostics::reset();
    }
    CommandResult::OkWith(CommandValue::Json(report.to_json()))
}

/// `GET_STATE { SERVER }`: the whole-server snapshot in the `ATTACHED`
/// `SessionSnapshot` shape, focused on the most recently touched session.
pub(crate) fn handle_get_state(
    state: &SharedState,
    viewer: Option<ClientId>,
    scope: &StateScope,
) -> CommandResult {
    match scope {
        StateScope::Server => {
            let snapshot = state.with_mut(|s| {
                let focus = s
                    .most_recently_touched_session()
                    .or_else(|| s.fallback_session());
                let mut snapshot = focus
                    .and_then(|sid| s.build_session_snapshot(sid))
                    .unwrap_or_else(empty_session_snapshot);
                // An empty registry still reports listeners for `phux doctor`.
                if snapshot.listeners().is_none() && s.has_remote_listener_report() {
                    snapshot = snapshot.with_listeners(s.remote_listeners().clone());
                }
                // L1 §7.3: the viewer's journal head at the cut, read under
                // the snapshot's lock.
                let snapshot = snapshot.with_journal_head(Some(s.journal_head_for(viewer)));
                // workload-auth §6: only what the viewer may INVENTORY.
                match viewer {
                    Some(viewer) => crate::policy::filter::filter_snapshot(
                        s,
                        viewer,
                        phux_protocol::kinds::Verb::Inventory,
                        snapshot,
                    ),
                    None => snapshot,
                }
            });
            CommandResult::OkWith(CommandValue::State(snapshot))
        }
        _ => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "unsupported GET_STATE scope".to_owned(),
        },
    }
}

/// `GET_STATE` with federation aggregation (L1 §9.1): on a hub, merge every
/// satellite's terminals (retagged `Satellite { host, id }`, queried
/// concurrently) into the local snapshot. Satellite sessions and windows do
/// not merge; each satellite contributes one `HostInventory` row instead,
/// sorted by host (no chaining). An unreachable satellite never fails the
/// aggregate: its row is marked unreachable and the caller gets one
/// uncorrelated `ERROR` before the result.
pub(crate) async fn handle_get_state_federated(
    state: &SharedState,
    viewer: ClientId,
    scope: &StateScope,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    let local = handle_get_state(state, Some(viewer), scope);
    if !matches!(scope, StateScope::Server) {
        return local;
    }
    // workload-auth §6: only the satellites the viewer may inventory are
    // asked, so another host's name and health never reach it.
    let relays: Vec<_> = state.with(|s| {
        s.hub_relays_all()
            .into_iter()
            .filter(|relay| {
                crate::policy::filter::admits_satellite(
                    s,
                    viewer,
                    phux_protocol::kinds::Verb::Inventory,
                    relay.host().as_str(),
                )
            })
            .collect()
    });
    if relays.is_empty() {
        // Non-hub server (or hub with an empty table): the local snapshot
        // is the whole truth, and its empty `hosts` says so.
        return local;
    }
    let CommandResult::OkWith(CommandValue::State(mut snapshot)) = local else {
        return local;
    };
    // Query every satellite concurrently: the aggregate's latency bound
    // is one relay deadline, not one per satellite.
    let queries = relays.into_iter().map(|relay| async move {
        let result = relay
            .command(Command::GetState {
                scope: StateScope::Server,
            })
            .await;
        (relay.host().clone(), result)
    });
    let mut hosts = Vec::new();
    for (host, result) in futures_util::future::join_all(queries).await {
        hosts.push(fold_satellite_state(state, &mut snapshot, host, result, out_tx).await);
    }
    hosts.sort_by(|a, b| a.host.as_str().cmp(b.host.as_str()));
    mirror_federated_agent_metadata(state, &snapshot);
    // workload-auth §6: the satellites' rows and Terminals, filtered too.
    let snapshot = state.with(|s| {
        crate::policy::filter::filter_snapshot(
            s,
            viewer,
            phux_protocol::kinds::Verb::Inventory,
            snapshot.with_hosts(hosts),
        )
    });
    CommandResult::OkWith(CommandValue::State(snapshot))
}

/// Ask each satellite link to mirror the agent allowlist for every terminal
/// the aggregate just listed (ADR-0136). A terminal the inventory does not
/// name is mirrored later, when a consumer subscribes or reads it.
pub(super) fn mirror_federated_agent_metadata(
    state: &SharedState,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) {
    for resource in &snapshot.resources {
        if !resource.kind.is_terminal() {
            continue;
        }
        let phux_protocol::ids::ResourceId::Satellite { host, id } = &resource.id else {
            continue;
        };
        if let Some(relay) = state.with(|s| s.hub_relay(host)) {
            relay.mirror_terminal(*id);
        }
    }
}

/// Fold one satellite's `GET_STATE` answer into the hub's aggregate: merge
/// its resources into `snapshot` and return its inventory row. An error
/// answer becomes an unreachable row plus the un-correlated degradation
/// `ERROR` pushed ahead of the `COMMAND_RESULT` (L1 §9.1).
pub(super) async fn fold_satellite_state(
    state: &SharedState,
    snapshot: &mut phux_protocol::wire::info::SessionSnapshot,
    host: phux_protocol::ids::SatelliteHost,
    result: CommandResult,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> phux_protocol::wire::info::HostInventory {
    match result {
        CommandResult::OkWith(CommandValue::State(sat)) => {
            let inventory = satellite_host_inventory(&host, &sat);
            let holders = state.with(|s| {
                sat.resources
                    .iter()
                    .filter_map(|pane| pane.id.local_id())
                    .filter_map(|id| {
                        s.satellite_lease_holder(&host, id)
                            .map(|holder| (id, holder))
                    })
                    .collect::<std::collections::HashMap<_, _>>()
            });
            merge_satellite_resources(snapshot, &host, sat.resources, &holders);
            inventory
        }
        CommandResult::Error { code, message } => {
            debug!(
                satellite = %host,
                ?code,
                %message,
                "GET_STATE aggregation: satellite contributes nothing"
            );
            let row = phux_protocol::wire::info::HostInventory::unreachable(host, message.clone());
            // Observable degradation, not silence: the same un-correlated
            // typed ERROR shape the relay uses for teardown notification.
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::Error {
                    request_id: None,
                    code,
                    message,
                }))
                .await;
            row
        }
        other => {
            warn!(
                satellite = %host,
                ?other,
                "GET_STATE aggregation: unexpected satellite result shape; skipping"
            );
            phux_protocol::wire::info::HostInventory::unreachable(
                host,
                "satellite answered GET_STATE with an unexpected result",
            )
        }
    }
}

/// Append a satellite's resources to the hub's aggregate, re-tagged into the
/// hub's id space (L1 §9.1). A Satellite-tagged id is dropped (no chaining).
/// The satellite's window and client ids are its own: `window_id` becomes
/// `WindowId(0)` (no hub window owns the row; the satellite's windows are not
/// aggregated), the input-lease holder becomes the hub consumer in
/// `holders` (the hub's lease ledger) or a foreign id, and viewers become
/// foreign ids, so none can match a hub window or impersonate a hub client.
pub(super) fn merge_satellite_resources(
    snapshot: &mut phux_protocol::wire::info::SessionSnapshot,
    host: &phux_protocol::ids::SatelliteHost,
    resources: Vec<phux_protocol::wire::info::ResourceInfo>,
    holders: &std::collections::HashMap<u32, ClientId>,
) {
    use crate::hub::relay::foreign_client;
    for mut pane in resources {
        let Some(id) = retag_satellite_resource_id(host, Some(&pane.id)) else {
            warn!(
                satellite = %host,
                pane = %pane.id,
                "satellite listed a Satellite-tagged terminal; dropping (no chaining)"
            );
            continue;
        };
        let holder = pane
            .id
            .local_id()
            .and_then(|local| holders.get(&local).copied());
        pane.id = id;
        // ADR-0104: a parent retags by the same rule; a chained one drops.
        pane.parent = retag_satellite_resource_id(host, pane.parent.as_ref());
        pane.window_id = phux_protocol::WindowId::new(0);
        pane.input_holder = pane.input_holder.map(|reported| {
            holder.map_or_else(|| foreign_client(reported), super::super::wire_client)
        });
        for viewer in &mut pane.viewers {
            *viewer = foreign_client(*viewer);
        }
        snapshot.resources.push(pane);
    }
}

/// The inventory row for a satellite that answered: one entry per session it
/// reported, under its own satellite-local id.
pub(super) fn satellite_host_inventory(
    host: &phux_protocol::ids::SatelliteHost,
    sat: &phux_protocol::wire::info::SessionSnapshot,
) -> phux_protocol::wire::info::HostInventory {
    let sessions = sat
        .sessions
        .iter()
        .map(|session| satellite_host_session(host, sat, session))
        .collect();
    phux_protocol::wire::info::HostInventory::reachable(host.clone(), sessions)
}

/// One satellite session's inventory entry, with its pane count and its
/// active pane derived from the satellite's own windows and resources.
pub(super) fn satellite_host_session(
    host: &phux_protocol::ids::SatelliteHost,
    sat: &phux_protocol::wire::info::SessionSnapshot,
    session: &phux_protocol::wire::info::SessionInfo,
) -> phux_protocol::wire::info::HostSessionInfo {
    phux_protocol::wire::info::HostSessionInfo::new(session.id, session.name.clone())
        .with_created_at_unix_secs(session.created_at_unix_secs)
        .with_window_count(session.window_count)
        .with_pane_count(session_pane_count(sat, session.id))
        .with_attached_client_count(session.attached_client_count)
        .with_active_resource(session_active_resource(host, sat, session))
}

/// Terminal-kind resources across a satellite session's windows.
pub(super) fn session_pane_count(
    sat: &phux_protocol::wire::info::SessionSnapshot,
    session: phux_protocol::SessionId,
) -> u16 {
    let panes = sat
        .resources
        .iter()
        .filter(|pane| pane.kind.is_terminal() && window_in_session(sat, pane.window_id, session))
        .count();
    u16::try_from(panes).unwrap_or(u16::MAX)
}

pub(super) fn window_in_session(
    sat: &phux_protocol::wire::info::SessionSnapshot,
    window: phux_protocol::WindowId,
    session: phux_protocol::SessionId,
) -> bool {
    sat.windows
        .iter()
        .any(|w| w.id == window && w.session_id == session)
}

/// A satellite session's remembered focused pane, re-tagged for the hub:
/// the session's reported recent input target, else (from a satellite that
/// predates it) the active window's active resource, falling back to the
/// session's first window and then to that window's first Terminal.
pub(super) fn session_active_resource(
    host: &phux_protocol::ids::SatelliteHost,
    sat: &phux_protocol::wire::info::SessionSnapshot,
    session: &phux_protocol::wire::info::SessionInfo,
) -> Option<phux_protocol::ids::ResourceId> {
    if let Some(recent) = &session.recent_input {
        return retag_satellite_resource_id(host, Some(recent));
    }
    let window = session
        .active_window
        .and_then(|id| sat.windows.iter().find(|w| w.id == id))
        .or_else(|| sat.windows.iter().find(|w| w.session_id == session.id))?;
    let local = window.active_resource.clone().or_else(|| {
        sat.resources
            .iter()
            .find(|pane| pane.kind.is_terminal() && pane.window_id == window.id)
            .map(|pane| pane.id.clone())
    });
    retag_satellite_resource_id(host, local.as_ref())
}

/// Retag a satellite's `Local { id }` as `Satellite { host, id }`. A
/// `Satellite`-tagged id has no hub-side form: hub-and-spoke does not chain
/// (L1 §9.1).
pub(super) fn retag_satellite_resource_id(
    host: &phux_protocol::ids::SatelliteHost,
    id: Option<&phux_protocol::ids::ResourceId>,
) -> Option<phux_protocol::ids::ResourceId> {
    match id? {
        phux_protocol::ids::ResourceId::Local { id } => {
            Some(phux_protocol::ids::ResourceId::satellite(host.clone(), *id))
        }
        phux_protocol::ids::ResourceId::Satellite { .. } => None,
    }
}
