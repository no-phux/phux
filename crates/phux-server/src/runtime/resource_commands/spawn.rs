//! Spawning an `AgentSession` under its Terminal parent, locally or relayed
//! to a satellite.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private submodule re-exported by resource_commands at crate visibility"
)]

use phux_protocol::caps::BootstrapLimits;
use phux_protocol::ids::ResourceKind;
use phux_protocol::wire::frame::{AgentEvent, FrameKind, SpawnError, SpawnResource};
use phux_resource::agent_session::AgentSessionResource as _;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::attach_agent_session;
use crate::mailbox::Outbound;
use crate::resource::agent_session::AgentSessionActor;
use crate::runtime::attach::refuse_spawn;
use crate::state::{ClientId, SharedState};
/// Validate parent ownership and kind in the same state borrow that registers
/// the child, before allocating any resource or publishing its identity.
fn validate_agent_parent(
    state: &crate::state::ServerState,
    parent: phux_core::ids::ResourceId,
) -> Result<(), SpawnError> {
    if state.native_owned_terminal(parent) {
        return Err(SpawnError::SpawnFailed(
            "an SDK-owned guardian reserves one native source; external hook sources cannot bind it".into(),
        ));
    }
    match state
        .registry()
        .resource(parent)
        .map(|resource| resource.kind)
    {
        Some(phux_core::ResourceKind::Terminal) => Ok(()),
        Some(_) => Err(SpawnError::ParentKindMismatch),
        None => Err(SpawnError::ParentNotFound),
    }
}

async fn bind_agent_stream(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    handle: &crate::resource::ResourceHandle,
) {
    if let Some(parent_handle) = state.with(|s| s.resource_handle(parent).cloned())
        && let Ok(facet) = handle.agent_session()
    {
        let _ = parent_handle
            .control
            .send(crate::resource::ControlRequest::BindAgentSession {
                append: facet.append.clone(),
            })
            .await;
    }
}

/// Handle a `SPAWN_RESOURCE` whose kind is `AgentSession` (ADR-0103 §1).
/// The parent must be a live local Terminal (an `AgentSession` parent is
/// `ParentKindMismatch`, ADR-0104 §5); a satellite-addressed spawn is relayed
/// with the parent reduced to the satellite's `Local` space (ADR-0104 §6).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) async fn spawn_agent_session(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    resource: &SpawnResource,
    satellite: Option<&phux_protocol::ids::SatelliteHost>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    bootstrap_limits: BootstrapLimits,
    connection_token: &CancellationToken,
) {
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

    if let Some(host) = satellite {
        relay_agent_session_spawn(
            state, client_id, request_id, resource, host, &parent, out_tx,
        )
        .await;
        return;
    }

    // A `Satellite` parent without a `satellite` route is not ours.
    let Some((core_parent, wire_parent)) = state.with(|s| {
        parent
            .is_local()
            .then(|| s.resource_from_wire(&parent))
            .flatten()
            .map(|core| (core, parent.clone()))
    }) else {
        debug!(?client_id, request_id, %parent, "SPAWN_RESOURCE: agent session parent not found");
        refuse_spawn(out_tx, request_id, SpawnError::ParentNotFound).await;
        return;
    };

    let log_bytes = state.with(crate::state::ServerState::agent_log_bytes);
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
        let facet = phux_core::resource::AgentFacet {
            provider: provider.to_owned(),
            native_id: resource.native_id.clone(),
            state: None,
        };
        validate_agent_parent(s, core_parent)?;
        let actor = s.actor_for(client_id);
        let core = s
            .create_agent_session(core_parent, &facet, &actor)
            .map_err(|_| SpawnError::ParentNotFound)?;
        let _ = s.spawn_resource_actor(core, bundle.handle.clone(), token, bundle.actor.run());
        let wire = s.intern_resource_wire(core);
        // Journaled before the exit watcher exists (ADR-0123), and fanned out
        // to the parent's subscribers too: nobody can be subscribed to a
        // resource they are hearing about for the first time.
        let announcement = crate::state::EventRecord::new(
            Some(wire.clone()),
            AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent: Some(wire_parent.clone()),
            },
        )
        .with_parent(Some(wire_parent.clone()))
        .with_actor(Some(client_id))
        .with_operation_id(resource.idempotency_key);
        let _ = s.record_and_fanout(announcement);
        // ADR-0126: the key binds in the step that registers the session.
        crate::runtime::idempotent_create::bind_spawned(s, resource.idempotency_key, &wire);
        // ADR-0109: provenance before the spawner's own subscription.
        s.record_spawn(core, client_id);
        s.subscribe_resource(client_id, core, Some(out_tx.clone()));
        // ADR-0127: a session attached as `VIEWER` watches its spawns too.
        s.mark_if_viewer_session(client_id, &wire);
        Ok((core, wire))
    });
    let (core_session, wire_session) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            refuse_spawn(out_tx, request_id, error).await;
            return;
        }
    };

    crate::runtime::client::spawn_resource_exit_watcher(
        state.clone(),
        core_session,
        Some(bundle.exit_notify),
        root_token.clone(),
        None,
    );

    // ADR-0103 §6: the parent's `REPORT_AGENT_STATE` now lands on this stream.
    bind_agent_stream(state, core_parent, &bundle.handle).await;

    let instance = resource
        .bind_instance
        .then(|| state.with(|s| s.idspace.instance()));
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result: crate::runtime::attach::spawned_result(wire_session.clone(), instance),
        }))
        .await;
    // The subscription alone forwards nothing: run the (empty) bootstrap and
    // live pump the `ATTACH_RESOURCE` path runs.
    let _ = attach_agent_session(
        state,
        client_id,
        &wire_session,
        core_session,
        &bundle.handle,
        out_tx,
        bootstrap_limits,
        connection_token,
        None,
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
    crate::runtime::attach::dispatch_satellite_spawn(
        state,
        client_id,
        out_tx,
        request_id,
        host,
        Ok(spawn),
    )
    .await;
}
