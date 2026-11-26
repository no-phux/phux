//! Spawning an `AgentSession` under its agent-hosting parent, locally or
//! relayed to a satellite.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module of the agent session app"
)]

use phux_agent_session::session::AgentSessionResource as _;
use phux_protocol::ids::ResourceKind;
use phux_protocol::wire::frame::{SpawnError, SpawnResource};
use tracing::debug;

use super::producer::{SpawnFinish, SpawnedChild, finish_spawn, register_spawned_child};
use crate::mailbox::Outbound;
use crate::runtime::apps::agent_session::attach_agent_session;
use crate::runtime::spawn::ResourceSpawn;
use crate::runtime::spawn_reply::{dispatch_satellite_spawn, refuse_spawn};
use crate::state::{ClientId, SharedState};
use phux_agent_session::session::AgentSessionActor;
/// Validate parent ownership and fitness in the same state borrow that
/// registers the child, before allocating any resource or publishing its
/// identity. A parent is fit when it
/// [supervises producers](crate::runtime::spawn::supervises_producers);
/// no kind is named here. The parent carries the activation the producer is
/// checked against (`verify_guardian_process`).
fn validate_agent_parent(
    state: &crate::state::ServerState,
    parent: phux_core::ids::ResourceId,
) -> Result<(), SpawnError> {
    if state.native_owned_terminal(parent) {
        return Err(SpawnError::SpawnFailed(
            "an SDK-owned guardian reserves one native source; external hook sources cannot bind it".into(),
        ));
    }
    let Some(handle) = state.resources().handle(parent) else {
        return Err(SpawnError::ParentNotFound);
    };
    if !crate::runtime::spawn::supervises_producers(handle) {
        return Err(SpawnError::ParentKindMismatch);
    }
    Ok(())
}

async fn bind_agent_stream(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    handle: &crate::resource::ResourceHandle,
) {
    let agent_parent = state.with(|s| {
        s.resources()
            .handle(parent)
            .and_then(|handle| handle.companion::<phux_agent_session::parent::AgentParent>())
            .cloned()
    });
    if let Some(agent_parent) = agent_parent
        && let Ok(facet) = handle.agent_session()
    {
        let _ = agent_parent
            .mailbox
            .send(
                phux_agent_session::parent::AgentParentRequest::BindSession {
                    append: facet.append.clone(),
                },
            )
            .await;
    }
}

/// Handle a `SPAWN_RESOURCE` whose kind is `AgentSession` (ADR-0103 §1).
/// The parent must be a live local resource that hosts agents (an
/// `AgentSession` parent is `ParentKindMismatch`, ADR-0104 §5); a
/// satellite-addressed spawn is relayed with the parent reduced to the
/// satellite's `Local` space (ADR-0104 §6).
pub(crate) async fn spawn_agent_session(spawn: ResourceSpawn<'_>) {
    let (state, client_id, request_id) = (spawn.state, spawn.client_id, spawn.request_id);
    let (resource, out_tx) = (spawn.resource, spawn.out_tx);
    let Some(provider) = resource.provider.as_deref().filter(|p| !p.is_empty()) else {
        refuse_spawn(
            out_tx,
            request_id,
            SpawnError::SpawnFailed("an agent session spawn must name a provider".to_owned()),
        )
        .await;
        return;
    };
    let Some(parent) = resource.parent.clone() else {
        refuse_spawn(out_tx, request_id, SpawnError::ParentNotFound).await;
        return;
    };

    if let Some(host) = spawn.satellite {
        relay_agent_session_spawn(
            state, client_id, request_id, resource, host, &parent, out_tx,
        )
        .await;
        return;
    }

    // A `Satellite` parent without a `satellite` route is not ours.
    let core_parent = state.with(|s| {
        parent
            .is_local()
            .then(|| s.idspace.resource_from_wire(&parent))
            .flatten()
    });
    let Some(core_parent) = core_parent else {
        debug!(?client_id, request_id, %parent, "SPAWN_RESOURCE: agent session parent not found");
        refuse_spawn(out_tx, request_id, SpawnError::ParentNotFound).await;
        return;
    };
    spawn_local_session(spawn, provider, core_parent, parent).await;
}

/// Validate `core_parent` and create the session's durable record under it,
/// in the caller's state borrow so the child registers in the same step.
fn create_session_record(
    s: &mut crate::state::ServerState,
    client_id: ClientId,
    core_parent: phux_core::ids::ResourceId,
    provider: &str,
    resource: &SpawnResource,
) -> Result<phux_core::ids::ResourceId, SpawnError> {
    let launch = phux_core::LaunchSpec::Agent(phux_core::resource::AgentLaunch {
        provider: provider.to_owned(),
        native_id: resource.native_id.clone(),
    });
    validate_agent_parent(s, core_parent)?;
    let actor = s.actor_for(client_id);
    s.create_child(
        core_parent,
        phux_core::NewResource::child(launch, None),
        &actor,
    )
    .map_err(|_| SpawnError::ParentNotFound)
}

/// Build, register and answer an admitted local session spawn under
/// `core_parent` (`wire_parent` on the wire).
async fn spawn_local_session(
    spawn: ResourceSpawn<'_>,
    provider: &str,
    core_parent: phux_core::ids::ResourceId,
    wire_parent: phux_protocol::ids::ResourceId,
) {
    let ResourceSpawn {
        state,
        client_id,
        request_id,
        resource,
        out_tx,
        root_token,
        limits: bootstrap_limits,
        connection_token,
        decodes,
        ..
    } = spawn;
    let log_bytes = state.with(|s| s.config().agent_log_bytes);
    let token = root_token.child_token();
    let bundle = AgentSessionActor::build(
        core_parent,
        provider,
        resource.native_id.as_deref(),
        token.clone(),
        log_bytes,
    );

    // One borrow: an id observed in an event must already resolve.
    let registered = state.with_mut(|s| {
        let core = create_session_record(s, client_id, core_parent, provider, resource)?;
        let child = SpawnedChild {
            core,
            handle: bundle.handle.clone(),
            token,
            actor: bundle.actor.run(),
            kind: ResourceKind::AgentSession,
            parent: &wire_parent,
            client_id,
            idempotency_key: resource.idempotency_key,
            out_tx,
        };
        register_spawned_child(s, child).map(|(wire, activation)| (core, wire, activation))
    });
    let (core_session, wire_session, activation) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            refuse_spawn(out_tx, request_id, error).await;
            return;
        }
    };

    let reservation = if decodes.contains(ResourceKind::AgentSession) {
        let host = crate::runtime::apps::StreamHost::new(state, client_id);
        host.next_bootstrap()
            .and_then(|bootstrap| host.reserve(core_session, bootstrap, connection_token))
    } else {
        None
    };
    let finish = SpawnFinish {
        state,
        request_id,
        out_tx,
        root_token,
        bind_instance: resource.bind_instance,
        decodes,
        kind: ResourceKind::AgentSession,
    };
    // ADR-0103 §6: the parent's `REPORT_AGENT_STATE` now lands on this stream.
    let bind = async || bind_agent_stream(state, core_parent, &bundle.handle).await;
    let attach = async || {
        let Some(reservation) = reservation else {
            return phux_protocol::wire::frame::CommandResult::error(
                phux_protocol::wire::frame::ErrorCode::InternalError,
                "spawner stream reservation unavailable",
            );
        };
        attach_agent_session(
            reservation,
            client_id,
            &wire_session,
            &bundle.handle,
            out_tx,
            bootstrap_limits,
            None,
        )
        .await
    };
    finish_spawn(
        finish,
        core_session,
        activation,
        &wire_session,
        bundle.exit_notify,
        bind,
        attach,
    )
    .await;
    debug!(
        ?client_id,
        request_id,
        session = %wire_session,
        provider,
        "SPAWN_RESOURCE: agent session bound to its parent"
    );
}

/// Forward a satellite-addressed session spawn over the owning hub link,
/// with the parent reduced to that satellite's `Local` space.
async fn relay_agent_session_spawn(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    resource: &SpawnResource,
    host: &phux_protocol::ids::SatelliteHost,
    parent: &phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let phux_protocol::ids::ResourceId::Satellite {
        host: parent_host,
        id,
    } = parent
    else {
        refuse_spawn(
            out_tx,
            request_id,
            SpawnError::SpawnFailed(
                "a satellite-addressed session must name a parent on that satellite".to_owned(),
            ),
        )
        .await;
        return;
    };
    if parent_host != host {
        refuse_spawn(
            out_tx,
            request_id,
            SpawnError::SpawnFailed(
                "the parent belongs to a different satellite than the spawn".to_owned(),
            ),
        )
        .await;
        return;
    }
    let mut forwarded = resource.clone();
    forwarded.parent = Some(phux_protocol::ids::ResourceId::local(*id));
    let spawn = crate::hub::relay::SatelliteSpawn {
        group: crate::state::DEFAULT_GROUP_ID,
        command: None,
        cwd: None,
        env: None,
        term: None,
        owner_terminal: None,
        initial_size: None,
        resource: Some(Box::new(forwarded)),
    };
    dispatch_satellite_spawn(state, client_id, out_tx, request_id, host, Ok(spawn)).await;
}
