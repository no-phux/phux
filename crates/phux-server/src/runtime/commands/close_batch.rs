//! Close batch: extracted from the shared module.
use super::{
    ClientId, Command, CommandResult, CommandValue, DetachReason, ErrorCode, FrameKind, Outbound,
    ServerInterceptedKey, SharedState, debug, kill_attribution,
};

/// Handle `KILL_RESOURCES` (L1 §5.2): close every local id under one lock,
/// then relay each satellite's part and await every answer. An unkeyed batch
/// answers `OK` (skips and relay failures are logged); a keyed batch
/// (L1 §5.1.1) answers one outcome per id, merged across hops.
pub(crate) async fn handle_kill_terminals(
    state: &SharedState,
    client_id: ClientId,
    ids: &[phux_protocol::ids::ResourceId],
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
) -> CommandResult {
    let partitions = satellite_partitions(ids);
    let attribution = kill_attribution(client_id, operation_id);
    let (killed, not_found) = kill_local_batch(state, ids, attribution, false);
    let relayed = futures_util::future::join_all(
        partitions
            .into_iter()
            .map(|(host, ids)| relay_kill_partition(state, client_id, host, ids, operation_id)),
    )
    .await;
    if operation_id.is_none() {
        warn_failed_partitions(&relayed);
        return CommandResult::Ok;
    }
    let mut results = super::super::keyed_ops::KillResults::default();
    for id in &killed {
        results.killed(id);
    }
    for id in &not_found {
        results.not_found(id);
    }
    for partition in &relayed {
        results.merge_host(&partition.host, &partition.ids, &partition.result);
    }
    results.into_result()
}

/// `CLOSE_TAB_RESOURCES` (L1 §5.2.2, ADR-0114): the atomic local close of
/// [`handle_kill_terminals`] that keeps keep-empty sessions. Unkeyed; the
/// satellite parts are relayed detached and not awaited.
pub(crate) fn handle_close_tab_resources(
    state: &SharedState,
    ids: &[phux_protocol::ids::ResourceId],
) -> CommandResult {
    relay_close_tab_partitions(state, ids);
    let _ = kill_local_batch(state, ids, crate::state::CloseAttribution::default(), true);
    CommandResult::Ok
}

/// A batch's satellite ids grouped by host, each kept as the caller wrote it.
pub(super) type KillPartitions = std::collections::BTreeMap<
    phux_protocol::ids::SatelliteHost,
    Vec<phux_protocol::ids::ResourceId>,
>;

pub(super) fn satellite_partitions(ids: &[phux_protocol::ids::ResourceId]) -> KillPartitions {
    let mut by_host = KillPartitions::new();
    for wire_id in ids {
        if let Some((host, _)) = crate::hub::relay::satellite_route(wire_id) {
            by_host.entry(host).or_default().push(wire_id.clone());
        }
    }
    by_host
}

/// Satellite-tagged ids rewritten into their host's `Local` space.
pub(super) fn satellite_local_ids(
    wire_ids: &[phux_protocol::ids::ResourceId],
) -> Vec<phux_protocol::ids::ResourceId> {
    wire_ids
        .iter()
        .filter_map(crate::hub::relay::satellite_route)
        .map(|(_, id)| phux_protocol::ids::ResourceId::local(id))
        .collect()
}

/// Close the local ids of a batch under one lock and return which named a
/// live resource and which named nothing. `preserve_keep_empty` is
/// `CLOSE_TAB_RESOURCES`.
pub(super) fn kill_local_batch(
    state: &SharedState,
    ids: &[phux_protocol::ids::ResourceId],
    attribution: crate::state::CloseAttribution,
    preserve_keep_empty: bool,
) -> (
    Vec<phux_protocol::ids::ResourceId>,
    Vec<phux_protocol::ids::ResourceId>,
) {
    let label = if preserve_keep_empty {
        "CLOSE_TAB_RESOURCES"
    } else {
        "KILL_RESOURCES"
    };
    let (killed, not_found, committed) = state.with_mut(|s| {
        let mut targets = Vec::with_capacity(ids.len());
        let (mut killed, mut not_found) = (Vec::new(), Vec::new());
        for wire_id in ids.iter().filter(|id| id.local_id().is_some()) {
            if let Some(core_id) = s.terminal_from_wire(wire_id) {
                targets.push(core_id);
                killed.push(wire_id.clone());
            } else {
                debug!(?wire_id, "{label}: unknown / dead id; skipping");
                not_found.push(wire_id.clone());
            }
        }
        // A batch naming every pane of a persistent session keeps its
        // destructive legacy meaning: the emptied session is deleted too.
        // CLOSE_TAB_RESOURCES keeps it (ADR-0114).
        let covered = if preserve_keep_empty {
            Vec::new()
        } else {
            s.sessions_covered_by(&targets)
        };
        // ADR-0104 §2: targets and their children close once, in this borrow,
        // and the commit is the teardown (L1 §5.2).
        let mut committed = super::super::committed_close::commit_close(
            s,
            &targets,
            phux_protocol::wire::frame::CloseReason::Killed,
            attribution,
        );
        let actor = attribution
            .actor
            .map_or_else(phux_core::Actor::owner, |client| s.actor_for(client));
        for name in s.delete_emptied_sessions(&covered, &actor) {
            let _ = s.metadata_broadcast(
                &phux_protocol::wire::frame::Scope::Global,
                ServerInterceptedKey::SessionKeepEmpty,
                &phux_protocol::wire::frame::encode_session_keep_empty(&name, false),
            );
        }
        let detached: Vec<_> = s
            .take_killed_sessions()
            .into_iter()
            .flat_map(|session| s.attached_clients_in_session(session))
            .collect();
        committed.extend_killed_clients(detached);
        debug!(
            requested = ids.len(),
            closed = committed.len(),
            "{label}: torn down local ids atomically"
        );
        (killed, not_found, committed)
    });
    super::super::committed_close::announce(state, committed);
    (killed, not_found)
}

/// `CLOSE_TAB_RESOURCES`'s satellite ids, forwarded detached to each host as
/// that host's own batch of the same command.
pub(super) fn relay_close_tab_partitions(
    state: &SharedState,
    ids: &[phux_protocol::ids::ResourceId],
) {
    for (host, wire_ids) in satellite_partitions(ids) {
        let local_ids = satellite_local_ids(&wire_ids);
        match state.with(|s| s.hub_relay(&host)) {
            Some(relay) => {
                debug!(
                    satellite = %host,
                    count = local_ids.len(),
                    "CLOSE_TAB_RESOURCES: relaying satellite partition"
                );
                relay.command_detached(Command::CloseTabResources { ids: local_ids });
            }
            None => {
                debug!(
                    satellite = %host,
                    "CLOSE_TAB_RESOURCES: no route to satellite; skipping its ids"
                );
            }
        }
    }
}

/// One satellite's part of a batch and its host's answer.
pub(super) struct KillPartition {
    pub(super) host: phux_protocol::ids::SatelliteHost,
    pub(super) ids: Vec<phux_protocol::ids::ResourceId>,
    pub(super) result: CommandResult,
}

/// Relay one satellite's part of a batch as that satellite's own
/// `KILL_RESOURCES`, under the batch's key. A server with no route to `host`
/// answers for it.
pub(super) async fn relay_kill_partition(
    state: &SharedState,
    client_id: ClientId,
    host: phux_protocol::ids::SatelliteHost,
    ids: Vec<phux_protocol::ids::ResourceId>,
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
) -> KillPartition {
    let command = Command::KillResources {
        ids: satellite_local_ids(&ids),
        operation_id,
    };
    let result = match state.with(|s| s.hub_relay(&host)) {
        Some(relay) => relay.command_from(command, client_id).await,
        None => CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!(
                "no satellite route to {host}: this server is not a federation hub for that host"
            ),
        },
    };
    KillPartition { host, ids, result }
}

/// An unkeyed batch keeps its `OK` reply, so a satellite part that failed is
/// said here instead.
pub(super) fn warn_failed_partitions(relayed: &[KillPartition]) {
    for partition in relayed {
        if let CommandResult::Error { code, message } = &partition.result {
            tracing::warn!(
                satellite = %partition.host,
                ?code,
                count = partition.ids.len(),
                %message,
                "KILL_RESOURCES: a satellite part was not killed; the unkeyed reply stays OK"
            );
        }
    }
}

/// Force-detach the session-attached clients of `session` (all when `None`)
/// for `phux detach`, answering the count as JSON. An unknown session
/// detaches nobody. `ATTACH_RESOURCE` subscribers are not swept.
pub(crate) fn handle_detach_clients(state: &SharedState, session: Option<&str>) -> CommandResult {
    let targets = state.with(|s| s.attached_clients_to_detach(session));
    let count = targets.len();
    for (client_id, tx) in targets {
        // `try_send`: a wedged mailbox is the stuck client this verb exists to
        // clear, so never await its capacity; the teardown still runs.
        let _ = tx.try_send(Outbound::Frame(FrameKind::Detached {
            reason: Some(DetachReason::Requested),
            message: "detached by `phux detach`".to_owned(),
        }));
        super::super::client::detach_and_release_consumer_state(state, client_id);
    }
    debug!(
        ?session,
        count, "DETACH_CLIENTS: force-detached clients from outside the attach UI"
    );
    CommandResult::OkWith(CommandValue::Json(count.to_string()))
}
