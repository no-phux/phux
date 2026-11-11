//! Spawning a document resource under the resource its producer runs in.

use phux_protocol::wire::frame::{AgentEvent, FrameKind, SpawnError};
use phux_tree::DocumentActor;
use tracing::debug;

use super::{DocumentApi, attach_document};
use crate::mailbox::Outbound;
use crate::runtime::apps::ResourceSpawn;
use crate::runtime::spawn_reply::{refuse_spawn, spawned_result};

/// The target, wire parent, and core parent a document spawn binds to, or
/// why it is refused. The spawner must be a connection that may publish;
/// the parent must be live and local, and is checked fit when the document
/// registers; `provider` names what the producer observes.
fn admit<'a, A: DocumentApi>(
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
        return Err(SpawnError::SpawnFailed(format!(
            "a {} spawn must name what its producer observes",
            A::LABEL
        )));
    };
    let Some(parent) = spawn.resource.parent.clone() else {
        return Err(SpawnError::ParentNotFound);
    };
    if spawn.satellite.is_some() || !parent.is_local() {
        return Err(SpawnError::SpawnFailed(format!(
            "a {} is local to the machine its producer runs on",
            A::LABEL
        )));
    }
    // The spawner becomes the producer, so it must be a connection that may
    // publish: one that never could would open a document nothing feeds.
    if !spawn.state.with(|s| s.client_may_produce(spawn.client_id)) {
        return Err(SpawnError::SpawnFailed(format!(
            "opening a {} requires a local owner connection, which may publish to it",
            A::LABEL
        )));
    }
    let Some(core_parent) = spawn.state.with(|s| s.resource_from_wire(&parent)) else {
        debug!(client_id = ?spawn.client_id, request_id = spawn.request_id, %parent, "SPAWN_RESOURCE: document parent not found");
        return Err(SpawnError::ParentNotFound);
    };
    Ok((target, parent, core_parent))
}

/// Handle a `SPAWN_RESOURCE` of document kind `A`.
pub(crate) async fn spawn_document<A: DocumentApi>(spawn: ResourceSpawn<'_>) {
    let (target, parent, core_parent) = match admit::<A>(&spawn) {
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
        decodes,
        ..
    } = spawn;

    let token = root_token.child_token();
    let bundle = DocumentActor::<A>::build(core_parent, target, token.clone());
    let handle = super::with_control(bundle.handle.clone(), resource.control);
    let registered = state.with_mut(|s| {
        let Some(parent_handle) = s.resource_handle(core_parent) else {
            return Err(SpawnError::ParentNotFound);
        };
        // The parent supervises the producer, as an agent session's does.
        if !crate::runtime::apps::spawn::supervises_producers(parent_handle) {
            return Err(SpawnError::ParentKindMismatch);
        }
        let actor = s.actor_for(client_id);
        let name = resource.native_id.as_deref();
        let core =
            A::create(s, core_parent, target, name, &actor).map_err(|refusal| match refusal {
                phux_core::Refusal::InvalidArgument { field: "name", .. } => {
                    SpawnError::SpawnFailed(refusal.to_string())
                }
                _ => SpawnError::ParentKindMismatch,
            })?;
        let wire = s
            .spawn_resource_actor(core, handle, token, bundle.actor.run())
            .map_err(|error| {
                s.reap_resource(core);
                SpawnError::SpawnFailed(error.to_string())
            })?;
        let announcement = crate::state::EventRecord::new(
            Some(wire.clone()),
            AgentEvent::ResourceSpawned {
                kind: A::WIRE_KIND,
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
    let (core_doc, wire_doc) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            refuse_spawn(out_tx, request_id, error).await;
            return;
        }
    };
    crate::runtime::connections::spawn_resource_exit_watcher(
        state.clone(),
        core_doc,
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
            result: spawned_result(wire_doc.clone(), instance),
        }))
        .await;
    // The spawner watches its own document: run the (empty) bootstrap and
    // the live pump the `ATTACH_RESOURCE` path runs. One whose `HELLO` does
    // not list the kind stays its producer, streamed nothing.
    if decodes.contains(A::WIRE_KIND) {
        let _ = attach_document::<A>(
            state,
            client_id,
            &wire_doc,
            core_doc,
            &bundle.handle,
            out_tx,
            bootstrap_limits,
            connection_token,
        )
        .await;
    }
    debug!(?client_id, request_id, resource = %wire_doc, kind = A::LABEL, target, "SPAWN_RESOURCE: document bound to its parent");
}
