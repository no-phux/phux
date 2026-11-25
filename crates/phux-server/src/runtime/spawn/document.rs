//! Spawning a document resource under the resource its producer runs in.

use crate::runtime::apps::document::engine::DocumentActor;
use phux_protocol::wire::frame::SpawnError;
use tracing::debug;

use crate::runtime::apps::document::{DocumentApi, attach_document};
use crate::runtime::spawn::ResourceSpawn;
use super::producer::{
    SpawnFinish, SpawnedChild, finish_spawn, register_spawned_child,
};
use crate::runtime::spawn_reply::refuse_spawn;

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
    let Some(core_parent) = spawn.state.with(|s| s.idspace.resource_from_wire(&parent)) else {
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
    let handle = crate::runtime::apps::document::with_control(bundle.handle.clone(), resource.control);
    let registered = state.with_mut(|s| {
        let Some(parent_handle) = s.resources().handle(core_parent) else {
            return Err(SpawnError::ParentNotFound);
        };
        // The parent supervises the producer, as an agent session's does.
        if !crate::runtime::spawn::supervises_producers(parent_handle) {
            return Err(SpawnError::ParentKindMismatch);
        }
        let actor = s.actor_for(client_id);
        let name = resource.native_id.as_deref();
        let core =
            s.create_child(core_parent, A::creation(target, name), &actor).map_err(|refusal| match refusal {
                phux_core::Refusal::NotFound => SpawnError::ParentNotFound,
                phux_core::Refusal::InvalidArgument { field: "name", .. } => {
                    SpawnError::SpawnFailed(refusal.to_string())
                }
                _ => SpawnError::ParentKindMismatch,
            })?;
        let child = SpawnedChild {
            core,
            handle,
            token,
            actor: bundle.actor.run(),
            kind: A::WIRE_KIND,
            parent: &parent,
            client_id,
            idempotency_key: resource.idempotency_key,
            out_tx,
        };
        register_spawned_child(s, child).map(|(wire, activation)| (core, wire, activation))
    });
    let (core_doc, wire_doc, activation) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            refuse_spawn(out_tx, request_id, error).await;
            return;
        }
    };
    let finish = SpawnFinish {
        state,
        request_id,
        out_tx,
        root_token,
        bind_instance: resource.bind_instance,
        decodes,
        kind: A::WIRE_KIND,
    };
    // The spawner watches its own document.
    let attach = async || {
        attach_document::<A>(
            &crate::runtime::apps::StreamHost::new(state, client_id),
            client_id,
            &wire_doc,
            core_doc,
            &bundle.handle,
            out_tx,
            bootstrap_limits,
            connection_token,
        )
        .await
    };
    finish_spawn(
        finish,
        core_doc,
        activation,
        &wire_doc,
        bundle.exit_notify,
        async || {},
        attach,
    )
    .await;
    debug!(?client_id, request_id, resource = %wire_doc, kind = A::LABEL, target, "SPAWN_RESOURCE: document bound to its parent");
}
