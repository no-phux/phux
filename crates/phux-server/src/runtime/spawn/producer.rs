//! Producer resource registration, lifecycle handoff and publication.
use crate::resource::ResourceHandle;
use crate::runtime::spawn_reply::spawned_result;
use crate::state::{ClientId, Outbound, ServerState, SharedState};
use phux_protocol::caps::ResourceKindSet;
use phux_protocol::ids::{IdempotencyKey, ResourceId as WireResourceId, ResourceKind as WireKind};
use phux_protocol::wire::frame::{AgentEvent, CommandResult, FrameKind, SpawnError};
use std::future::Future;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
/// A producer child built and checked by its kind, ready to register.
pub(super) struct SpawnedChild<'a, F> {
    pub(super) core: phux_core::ids::ResourceId,
    pub(super) handle: ResourceHandle,
    pub(super) token: CancellationToken,
    pub(super) actor: F,
    pub(super) kind: WireKind,
    pub(super) parent: &'a WireResourceId,
    pub(super) client_id: ClientId,
    pub(super) idempotency_key: Option<IdempotencyKey>,
    pub(super) out_tx: &'a mpsc::Sender<Outbound>,
}

/// Register a spawned producer child in the borrow that created it, so an
/// id observed in an event already resolves: start its actor (reaping the
/// child when that fails), announce it, bind its idempotency key, and
/// subscribe the spawner to it.
pub(super) fn register_spawned_child<F>(
    s: &mut ServerState,
    child: SpawnedChild<'_, F>,
) -> Result<(WireResourceId, crate::state::ActivationToken), SpawnError>
where
    F: Future<Output = ()> + 'static,
{
    let SpawnedChild {
        core,
        handle,
        token,
        actor,
        kind,
        parent,
        client_id,
        idempotency_key,
        out_tx,
    } = child;
    let activation = s.activation_token(core).ok_or(SpawnError::ParentNotFound)?;
    let wire = s
        .spawn_resource_actor(core, handle, token, actor)
        .map_err(|error| {
            s.reap_resource(core);
            SpawnError::SpawnFailed(error.to_string())
        })?;
    // Journaled before the exit watcher exists (ADR-0123), and fanned out
    // to the parent's subscribers too: nobody can be subscribed to a
    // resource they are hearing about for the first time.
    let announcement = crate::state::EventRecord::new(
        Some(wire.clone()),
        AgentEvent::ResourceSpawned {
            kind,
            parent: Some(parent.clone()),
        },
    )
    .with_parent(Some(parent.clone()))
    .with_actor(Some(client_id))
    .with_operation_id(idempotency_key);
    let _ = s.record_and_fanout(announcement);
    // ADR-0126: the key binds in the step that registers the child.
    crate::runtime::idempotent_create::bind_spawned(s, idempotency_key, &wire);
    // ADR-0109: provenance before the spawner's own subscription.
    s.resources_mut().record_spawn(core, client_id);
    s.subscribe_resource(client_id, core, Some(out_tx.clone()));
    // ADR-0127: a session attached as `VIEWER` watches its spawns too.
    s.mark_if_viewer_session(client_id, &wire);
    Ok((wire, activation))
}

/// What answering a registered producer spawn needs.
pub(super) struct SpawnFinish<'a> {
    pub(super) state: &'a SharedState,
    pub(super) request_id: u32,
    pub(super) out_tx: &'a mpsc::Sender<Outbound>,
    pub(super) root_token: &'a CancellationToken,
    pub(super) bind_instance: bool,
    pub(super) decodes: ResourceKindSet,
    pub(super) kind: WireKind,
}

/// Watch the registered child's exit, run the kind's `bind` step, answer
/// the spawn, then stream the child to its spawner through `attach` when
/// its `HELLO` lists the kind. One that does not stays its producer,
/// streamed nothing. Both steps are closures so neither future is held
/// while the other runs.
pub(super) async fn finish_spawn(
    finish: SpawnFinish<'_>,
    core: phux_core::ids::ResourceId,
    activation: crate::state::ActivationToken,
    wire: &WireResourceId,
    exit_notify: oneshot::Receiver<phux_core::process::ExitOutcome>,
    bind: impl AsyncFnOnce(),
    attach: impl AsyncFnOnce() -> CommandResult,
) {
    let SpawnFinish {
        state,
        request_id,
        out_tx,
        root_token,
        bind_instance,
        decodes,
        kind,
    } = finish;
    crate::runtime::connections::spawn_resource_exit_watcher(
        state.clone(),
        core,
        activation,
        Some(exit_notify),
        root_token.clone(),
        None,
    );
    bind().await;
    let instance = bind_instance.then(|| state.with(|s| s.idspace.instance()));
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result: spawned_result(wire.clone(), instance),
        }))
        .await;
    // The subscription alone forwards nothing: run the (empty) bootstrap
    // and live pump the `ATTACH_RESOURCE` path runs.
    if decodes.contains(kind) {
        let _ = attach().await;
    }
}
