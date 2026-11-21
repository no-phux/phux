//! Terminal kill and close-tab handling, including satellite partitions.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;
use crate::runtime::committed_close::{CommittedClose, announce, commit_user_close};

/// The attribution a kill stamps on the `pane_closed` of everything it
/// closes: the sending connection and, when keyed, its `operation_id`.
pub(super) const fn kill_attribution(
    client_id: ClientId,
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
) -> crate::state::CloseAttribution {
    crate::state::CloseAttribution {
        actor: Some(client_id),
        operation_id,
    }
}

/// `KILL_RESOURCE`: close the pane through the same teardown a natural exit
/// takes, so `RESOURCE_CLOSED` still fires.
pub(super) fn handle_kill_terminal(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    attribution: crate::state::CloseAttribution,
) -> CommandResult {
    let admitted = state.with_mut(|s| {
        let id = s
            .idspace
            .resource_from_wire(terminal_id)
            .ok_or_else(|| terminal_not_found(terminal_id))?;
        commit_user_close(s, &[id], attribution)
    });
    finish_close(state, admitted)
}

/// `KILL_RESOURCE_IF` (ADR-0109): check and kill under one lock so
/// no attach lands between them. A refusal kills nothing.
pub(super) fn handle_kill_resource_if(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    precondition: &phux_protocol::wire::frame::KillPrecondition,
    attribution: crate::state::CloseAttribution,
) -> CommandResult {
    let admitted = state.with_mut(|s| {
        let core = s
            .admit_conditional_kill(terminal_id, precondition)
            .map_err(|refusal| kill_if_refusal(terminal_id, refusal))?;
        commit_user_close(s, &[core], attribution)
    });
    finish_close(state, admitted)
}

fn finish_close(
    state: &SharedState,
    admitted: Result<CommittedClose, CommandResult>,
) -> CommandResult {
    match admitted {
        Ok(committed) => {
            announce(state, committed);
            CommandResult::Ok
        }
        Err(refusal) => refusal,
    }
}

/// The typed refusal a `KILL_RESOURCE_IF` answers when it killed nothing.
fn kill_if_refusal(
    terminal_id: &phux_protocol::ids::ResourceId,
    refusal: crate::state::KillIfRefusal,
) -> CommandResult {
    match refusal {
        crate::state::KillIfRefusal::NotFound => terminal_not_found(terminal_id),
        crate::state::KillIfRefusal::Precondition(why) => CommandResult::error(
            ErrorCode::PreconditionFailed,
            format!("{terminal_id} not killed: {why}"),
        ),
    }
}

/// Handle `KILL_RESOURCES`: close every local id under one lock,
/// then relay each satellite's part and await every answer. An unkeyed batch
/// answers `OK` (skips and relay failures are logged); a keyed batch
/// answers one outcome per id, merged across hops.
pub(crate) async fn handle_kill_terminals(
    state: &SharedState,
    client_id: ClientId,
    ids: &[phux_protocol::ids::ResourceId],
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
) -> CommandResult {
    let attribution = kill_attribution(client_id, operation_id);
    let local = match kill_local_batch(state, ids, attribution, false) {
        Ok(local) => local,
        Err(refused) => return refused,
    };
    let partitions = satellite_partitions(ids);
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
    for id in &local.killed {
        results.killed(id);
    }
    for id in &local.not_found {
        results.not_found(id);
    }
    for partition in &relayed {
        results.merge_host(&partition.host, &partition.ids, &partition.result);
    }
    results.into_result()
}

/// `CLOSE_TAB_RESOURCES` (ADR-0114): the atomic local close of
/// [`handle_kill_terminals`] that keeps keep-empty sessions. Unkeyed; the
/// satellite parts are relayed detached and not awaited.
pub(crate) fn handle_close_tab_resources(
    state: &SharedState,
    ids: &[phux_protocol::ids::ResourceId],
) -> CommandResult {
    if let Err(refused) =
        kill_local_batch(state, ids, crate::state::CloseAttribution::default(), true)
    {
        return refused;
    }
    relay_close_tab_partitions(state, ids);
    CommandResult::Ok
}

/// A batch's satellite ids grouped by host, each kept as the caller wrote it.
type KillPartitions = std::collections::BTreeMap<
    phux_protocol::ids::SatelliteHost,
    Vec<phux_protocol::ids::ResourceId>,
>;

fn satellite_partitions(ids: &[phux_protocol::ids::ResourceId]) -> KillPartitions {
    let mut by_host = KillPartitions::new();
    for wire_id in ids {
        if let Some((host, _)) = crate::hub::relay::satellite_route(wire_id) {
            by_host.entry(host).or_default().push(wire_id.clone());
        }
    }
    by_host
}

/// Satellite-tagged ids rewritten into their host's `Local` space.
fn satellite_local_ids(
    wire_ids: &[phux_protocol::ids::ResourceId],
) -> Vec<phux_protocol::ids::ResourceId> {
    wire_ids
        .iter()
        .filter_map(crate::hub::relay::satellite_route)
        .map(|(_, id)| phux_protocol::ids::ResourceId::local(id))
        .collect()
}

#[derive(Default)]
struct LocalKillResults {
    killed: Vec<phux_protocol::ids::ResourceId>,
    not_found: Vec<phux_protocol::ids::ResourceId>,
}

/// Close the local ids of a batch under one lock and return which named a
/// live resource and which named nothing. `preserve_keep_empty` is
/// `CLOSE_TAB_RESOURCES`.
fn kill_local_batch(
    state: &SharedState,
    ids: &[phux_protocol::ids::ResourceId],
    attribution: crate::state::CloseAttribution,
    preserve_keep_empty: bool,
) -> Result<LocalKillResults, CommandResult> {
    let label = if preserve_keep_empty {
        "CLOSE_TAB_RESOURCES"
    } else {
        "KILL_RESOURCES"
    };
    let (results, committed) = state.with_mut(|s| {
        let mut targets = Vec::with_capacity(ids.len());
        let mut results = LocalKillResults::default();
        for wire_id in ids.iter().filter(|id| id.local_id().is_some()) {
            if let Some(core_id) = s.idspace.resource_from_wire(wire_id) {
                targets.push(core_id);
                results.killed.push(wire_id.clone());
            } else {
                debug!(?wire_id, "{label}: unknown / dead id; skipping");
                results.not_found.push(wire_id.clone());
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
        let mut committed = commit_user_close(s, &targets, attribution)?;
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
            .flat_map(|session| s.clients().attached_in_session(session))
            .collect();
        committed.extend_killed_clients(detached);
        debug!(
            requested = ids.len(),
            closed = committed.len(),
            "{label}: torn down local ids atomically"
        );
        Ok::<_, CommandResult>((results, committed))
    })?;
    announce(state, committed);
    Ok(results)
}

/// `CLOSE_TAB_RESOURCES`'s satellite ids, forwarded detached to each host as
/// that host's own batch of the same command.
fn relay_close_tab_partitions(state: &SharedState, ids: &[phux_protocol::ids::ResourceId]) {
    for (host, wire_ids) in satellite_partitions(ids) {
        let local_ids = satellite_local_ids(&wire_ids);
        match state.with(|s| s.federation().relay(&host)) {
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
struct KillPartition {
    pub(super) host: phux_protocol::ids::SatelliteHost,
    pub(super) ids: Vec<phux_protocol::ids::ResourceId>,
    pub(super) result: CommandResult,
}

/// Relay one satellite's part of a batch as that satellite's own
/// `KILL_RESOURCES`, under the batch's key. A server with no route to `host`
/// answers for it.
async fn relay_kill_partition(
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
    let result = match state.with(|s| s.federation().relay(&host)) {
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
fn warn_failed_partitions(relayed: &[KillPartition]) {
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
