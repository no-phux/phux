//! Retagging satellite ids and routing commands to the satellite they address.

use super::{
    Command, ResourceId, RolePolicy, SatelliteHost, ServerFeature, ServerFeatureExt,
    ServerFeatureExtSet, ServerFeatureSet, SpawnError, SpawnResult, warn,
};

/// Re-tag a satellite's spawn reply: `Local { id }` becomes
/// `Satellite { host, id }`, the instance token passes through (ADR-0109),
/// and a `Satellite`-tagged id becomes `SpawnFailed`.
pub(super) fn retag_spawn_result(host: &SatelliteHost, result: SpawnResult) -> SpawnResult {
    let Some(spawned) = result.spawned_id() else {
        return result;
    };
    let ResourceId::Local { id } = spawned else {
        warn!(
            satellite = %host,
            "satellite answered a spawn with a Satellite-tagged id; hub-and-spoke does not chain"
        );
        return SpawnResult::Err(SpawnError::SpawnFailed(
            "satellite returned a chained satellite id".to_owned(),
        ));
    };
    let id = ResourceId::satellite(host.clone(), *id);
    // A replayed reply stays replayed (ADR-0126).
    match (result.is_replayed(), result.instance()) {
        (true, instance) => SpawnResult::Replayed { id, instance },
        (false, Some(instance)) => SpawnResult::OkBound { id, instance },
        (false, None) => SpawnResult::Ok(id),
    }
}

/// The high bit a [`foreign_client`] id carries. Hub consumers' wire ids are
/// allocated upward from 1 and never reach it.
pub(super) const FOREIGN_CLIENT_BIT: u32 = 1 << 31;

/// A client id a satellite reported (one of its own connections, often the
/// hub's link itself), moved into a range hub client ids never reach, so it
/// can neither collide with nor impersonate a hub consumer.
pub(crate) const fn foreign_client(
    id: phux_protocol::ids::ClientId,
) -> phux_protocol::ids::ClientId {
    phux_protocol::ids::ClientId::new(FOREIGN_CLIENT_BIT | id.get())
}

/// Split a satellite-tagged wire id into its host and satellite-local id.
pub(crate) fn satellite_route(terminal_id: &ResourceId) -> Option<(SatelliteHost, u32)> {
    match terminal_id {
        ResourceId::Satellite { host, id } => Some((host.clone(), *id)),
        ResourceId::Local { .. } => None,
    }
}

/// Rebuild a filtered attach. A cursor is an agent resume; no cursor uses the
/// terminal constructor because that is the same no-cursor wire shape.
pub(super) const fn forwarded_attach(
    resource_id: ResourceId,
    role_policy: Option<RolePolicy>,
    after_seq: Option<u64>,
) -> Command {
    match after_seq {
        Some(after_seq) => Command::attach_agent_session(resource_id, role_policy, Some(after_seq)),
        None => Command::attach_terminal(resource_id, role_policy),
    }
}

/// The command a hub's link sends for a consumer's attach (ADR-0127).
///
/// The link is one identity shared by every consumer, so only a deliberate
/// takeover crosses, and only to a satellite advertising `ATTACH_ROLES`.
pub(crate) fn link_attach_command(
    command: Command,
    satellite: ServerFeatureSet,
    satellite_ext: ServerFeatureExtSet,
) -> Command {
    let Command::AttachResource {
        terminal_id,
        role_policy,
        agent_after_seq,
    } = command
    else {
        return command;
    };
    let role_policy = role_policy
        .filter(|policy| policy.takes_over() && satellite.contains(ServerFeature::AttachRoles));
    let agent_after_seq =
        agent_after_seq.filter(|_| satellite_ext.contains(ServerFeatureExt::AgentStreamResume));
    forwarded_attach(terminal_id, role_policy, agent_after_seq)
}

/// If `command` targets one satellite-owned terminal, the host and the
/// command rewritten to `Local` ids. `None` for local targets, hub-local
/// commands, and mixed batches.
pub(crate) fn route_to_satellite(command: &Command) -> Option<(SatelliteHost, Command)> {
    if let Command::AttachResource {
        terminal_id,
        role_policy,
        agent_after_seq,
    } = command
    {
        let (host, id) = satellite_route(terminal_id)?;
        // The consumer's intent rides unchanged; the link command is decided
        // at dispatch (ADR-0127).
        return Some((
            host,
            forwarded_attach(ResourceId::local(id), *role_policy, *agent_after_seq),
        ));
    }
    let (host, id) = satellite_route(single_target(command)?)?;
    // Everything but the target rides unchanged: a kill's precondition and
    // every `operation_id` cross verbatim (ADR-0109, L1 §9.1).
    let mut local = command.clone();
    *single_target_mut(&mut local)? = ResourceId::local(id);
    Some((host, local))
}

/// The one Terminal a command that relays verbatim addresses; `None` for
/// hub-local, batch-partitioned, or specially relayed commands.
macro_rules! single_target {
    ($command:expr) => {
        match $command {
            Command::DetachResource { terminal_id }
            | Command::GetScreen { terminal_id, .. }
            | Command::RouteInput { terminal_id, .. }
            | Command::GetTerminalState { terminal_id, .. }
            | Command::SubscribeResourceEvents { terminal_id, .. }
            | Command::AcquireInput { terminal_id, .. }
            | Command::ReleaseInput { terminal_id }
            | Command::SignalTerminal { terminal_id, .. }
            | Command::PutFile { terminal_id, .. }
            | Command::Checkout { terminal_id, .. }
            | Command::ReportAsked { terminal_id, .. }
            | Command::ReportAgentState { terminal_id, .. }
            | Command::KillResource { terminal_id, .. }
            | Command::KillResourceIf { terminal_id, .. }
            | Command::ApplyInput { terminal_id, .. } => Some(terminal_id),
            _ => None,
        }
    };
}

pub(super) const fn single_target(command: &Command) -> Option<&ResourceId> {
    single_target!(command)
}

pub(super) const fn single_target_mut(command: &mut Command) -> Option<&mut ResourceId> {
    single_target!(command)
}
