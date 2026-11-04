//! Spawning an `AccessibilityTree` under the Terminal its producer runs in.

use phux_protocol::wire::frame::{AgentEvent, FrameKind, SpawnError};
use phux_tree::TreeActor;
use tracing::debug;

use super::attach_tree;
use crate::mailbox::Outbound;
use crate::runtime::apps::ResourceSpawn;
use crate::runtime::spawn_reply::{refuse_spawn, spawned_result};

/// The application, wire parent, and core parent a tree spawn binds to, or
/// why it is refused. The parent must be a live local Terminal; `provider`
/// names the observed application.
fn admit<'a>(
    spawn: &ResourceSpawn<'a>,
) -> Result<
    (
        &'a str,
        phux_protocol::ids::ResourceId,
        phux_core::ids::ResourceId,
    ),
    SpawnError,
> {
    let Some(target) = spawn.resource.provider.as_deref().filter(|p| !p.is_empty()) else {
        return Err(SpawnError::SpawnFailed(
            "an accessibility tree spawn must name the application it observes".to_owned(),
        ));
    };
    let Some(parent) = spawn.resource.parent.clone() else {
        return Err(SpawnError::ParentNotFound);
    };
    if spawn.satellite.is_some() || !parent.is_local() {
        return Err(SpawnError::SpawnFailed(
            "an accessibility tree is local to the machine its producer runs on".to_owned(),
        ));
    }
    let Some(core_parent) = spawn.state.with(|s| s.resource_from_wire(&parent)) else {
        debug!(client_id = ?spawn.client_id, request_id = spawn.request_id, %parent, "SPAWN_RESOURCE: tree parent not found");
        return Err(SpawnError::ParentNotFound);
    };
    Ok((target, parent, core_parent))
}

/// Handle a `SPAWN_RESOURCE` whose kind is `AccessibilityTree`.
pub(crate) async fn spawn_tree(spawn: ResourceSpawn<'_>) {
    let (target, parent, core_parent) = match admit(&spawn) {
        Ok(admitted) => admitted,
        Err(error) => {
            refuse_spawn(spawn.out_tx, spawn.request_id, error).await;
            return;
        }
    };
    let ResourceSpawn {
        state,
        client_id,
        request_id,
        resource,
        out_tx,
        root_token,
        limits: bootstrap_limits,
        connection_token,
        ..
    } = spawn;

    let token = root_token.child_token();
    let bundle = TreeActor::build(core_parent, target, token.clone());
    let registered = state.with_mut(|s| {
        if s.resource_handle(core_parent).is_none() {
            return Err(SpawnError::ParentNotFound);
        }
        let actor = s.actor_for(client_id);
        let core = s
            .create_tree(core_parent, target, &actor)
            .map_err(|_| SpawnError::ParentKindMismatch)?;
        let _ = s.spawn_resource_actor(core, bundle.handle.clone(), token, bundle.actor.run());
        let wire = s.intern_resource_wire(core);
        let announcement = crate::state::EventRecord::new(
            Some(wire.clone()),
            AgentEvent::ResourceSpawned {
                kind: phux_protocol::ids::ResourceKind::AccessibilityTree,
                parent: Some(parent.clone()),
            },
        )
        .with_parent(Some(parent.clone()))
        .with_actor(Some(client_id))
        .with_operation_id(resource.idempotency_key);
        let _ = s.record_and_fanout(announcement);
        crate::runtime::idempotent_create::bind_spawned(s, resource.idempotency_key, &wire);
        s.record_spawn(core, client_id);
        s.subscribe_resource(client_id, core, Some(out_tx.clone()));
        s.mark_if_viewer_session(client_id, &wire);
        Ok((core, wire))
    });
    let (core_tree, wire_tree) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            refuse_spawn(out_tx, request_id, error).await;
            return;
        }
    };
    crate::runtime::connections::spawn_resource_exit_watcher(
        state.clone(),
        core_tree,
        Some(bundle.exit_notify),
        root_token.clone(),
        None,
    );
    let instance = resource
        .bind_instance
        .then(|| state.with(|s| s.idspace.instance()));
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result: spawned_result(wire_tree.clone(), instance),
        }))
        .await;
    // The spawner watches its own tree: run the (empty) bootstrap and the
    // live pump the `ATTACH_RESOURCE` path runs.
    let _ = attach_tree(
        state,
        client_id,
        &wire_tree,
        core_tree,
        &bundle.handle,
        out_tx,
        bootstrap_limits,
        connection_token,
    )
    .await;
    debug!(?client_id, request_id, tree = %wire_tree, target, "SPAWN_RESOURCE: accessibility tree bound to its parent");
}
