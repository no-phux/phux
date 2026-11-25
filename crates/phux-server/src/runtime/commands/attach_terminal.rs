//! Terminal attach, bootstrap, and subscription.

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::wire::frame::{Command, CommandResult, ControlAction, ErrorCode, RolePolicy};
use tokio_util::sync::CancellationToken;

use crate::resource::ResourceHandle;
use crate::runtime::commands::results::{
    AttachResourceFailure, terminal_not_found, wrong_resource_kind,
};
use crate::runtime::commands::routing::CommandContext;
use crate::state::{ClientId, Outbound, RoleEffects, SharedState};

/// Handle `ATTACH_RESOURCE`: subscribe the caller to one
/// Terminal's content stream, snapshot first, without a session `ATTACH`.
/// Idempotent: a re-attach re-sends a snapshot without a second pump. It
/// never resizes; interactive callers follow with `RESIZE_TERMINAL`.
#[allow(
    clippy::too_many_arguments,
    reason = "negotiated connection context threaded from dispatch"
)]
pub(super) async fn handle_attach_terminal(
    ctx: &CommandContext,
    terminal_id: &phux_protocol::ids::ResourceId,
    role: RolePolicy,
    agent_after_seq: Option<u64>,
) -> CommandResult {
    let CommandContext {
        state,
        client_id,
        out_tx,
        ..
    } = ctx;
    let client_id = *client_id;
    let subscription =
        match subscribe_attach_terminal(state, client_id, terminal_id, out_tx, Some(role)) {
            Ok(subscription) => subscription,
            Err(refusal) => return refusal,
        };
    let host = crate::runtime::apps::StreamHost::new(state, client_id);
    let Some(reservation) = host.reserve_next(subscription.core, &ctx.token) else {
        state.with_mut(|s| {
            if !subscription.was_subscribed { s.resources_mut().unsubscribe(client_id, subscription.core); }
            if subscription.was_viewer && !role.is_viewer() { s.clients_mut().mark_viewer(client_id, terminal_id, true); }
            s.undo_attach_takeover(client_id, subscription.core, subscription.effects);
        });
        return CommandResult::error(ErrorCode::InternalError, "stream admission unavailable");
    };
    let lease = reservation.lease.clone();
    let stream = crate::runtime::apps::StreamBootstrap {
        reservation, client_id, wire: terminal_id, handle: &subscription.handle,
        content_tx: out_tx, stream_id: crate::runtime::stream_id_from(client_id.0),
        client_caps: ctx.client_caps, profile: ctx.profile, limits: ctx.limits,
        after_seq: agent_after_seq,
    };
    let result = lease.run(async {
        announce_role_effects(&subscription, client_id).await;
        bootstrap_attach_terminal(stream).await
    }).await.unwrap_or_else(|| Err(AttachResourceFailure {
        generation: Some(std::sync::Arc::clone(&lease.last_seq)),
        ..AttachResourceFailure::internal("stream superseded while announcing role")
    }));
    let AttachSubscription { core, handle, effects, was_viewer, .. } = subscription;
    match result
    {
        Ok(()) => CommandResult::Ok,
        Err(failure) => {
            // The COMMAND path drops its subscription on failure and undoes
            // any lease seizure or role widening it made (ADR-0127).
            let restore = was_viewer && !role.is_viewer();
            let rollback = state.with_mut(|s| {
                let current = failure.generation.as_ref().is_some_and(|fence| s.resources().pump_matches(client_id, core, fence));
                if !current { return None; }
                s.resources_mut().unsubscribe(client_id, core);
                if restore {
                    s.clients_mut().mark_viewer(client_id, terminal_id, true);
                }
                let released = s.undo_attach_takeover(client_id, core, effects);
                Some((released, s.leases().holder(core)))
            });
            let Some((released, holder)) = rollback else {
                return CommandResult::error(failure.code, failure.message);
            };
            let lease = handle.companion::<crate::resource::companions::Leasable>();
            if restore
                && effects.role_changed
                && let Some(lease) = lease
            {
                lease
                    .lease_changed(crate::resource::LeaseChange {
                        input_holder: holder.map(super::super::wire_client),
                        action: ControlAction::RoleChanged,
                        actor: Some(super::super::wire_client(client_id)),
                    })
                    .await;
            }
            if released && let Some(lease) = lease {
                lease
                    .lease_changed(crate::resource::LeaseChange {
                        input_holder: None,
                        action: ControlAction::Released,
                        actor: Some(super::super::wire_client(client_id)),
                    })
                    .await;
            }
            CommandResult::error(failure.code, failure.message)
        }
    }
}

/// Bootstrap only the host-admitted exact source. The reservation is captured
/// before role announcements or transport retirement can suspend its caller.
pub(crate) async fn bootstrap_attach_terminal(stream: crate::runtime::apps::StreamBootstrap<'_>) -> Result<(), AttachResourceFailure> {
    let lease = stream.reservation.lease.clone();
    lease.run(crate::runtime::apps::app_for(stream.handle.kind).bootstrap(stream)).await
        .unwrap_or_else(|| Err(AttachResourceFailure::internal("stream reservation superseded")))
        .map_err(|mut failure| { failure.generation = Some(std::sync::Arc::clone(&lease.last_seq)); failure })
}

/// Resolve the wire terminal id and register the caller as an output
/// subscriber in one critical section.
///
/// The mailbox is remembered with the subscription because a session-less
/// subscriber has no other route for lifecycle fanout such as
/// `RESOURCE_CLOSED`. `role` (ADR-0127) applies in the same
/// critical section; `None` leaves the role untouched (a `STREAM_BIND`).
pub(crate) fn subscribe_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    role: Option<RolePolicy>,
) -> Result<AttachSubscription, CommandResult> {
    let not_found = || terminal_not_found(terminal_id);
    state.with_mut(|s| {
        let core = s
            .idspace
            .resource_from_wire(terminal_id)
            .ok_or_else(not_found)?;
        let handle = s.resources().handle(core).cloned().ok_or_else(not_found)?;
        // The lease is a Terminal-facet concept, so a takeover of
        // any other kind is refused the way `ACQUIRE_INPUT` refuses it.
        if role.is_some_and(RolePolicy::takes_over)
            && handle.kind != crate::resource::ResourceKind::Terminal
        {
            return Err(wrong_resource_kind(crate::resource::WrongResourceKind {
                required: crate::resource::ResourceKind::Terminal,
                actual: handle.kind,
            }));
        }
        let was_subscribed = s.resources().subscribers_for(core).contains(&client_id);
        let was_viewer = s.clients().is_viewer(client_id, terminal_id);
        s.subscribe_resource(client_id, core, Some(out_tx.clone()));
        let effects = role.map_or_else(RoleEffects::default, |policy| {
            s.apply_attach_role(client_id, terminal_id, Some(core), policy, was_subscribed)
        });
        Ok(AttachSubscription {
            core,
            handle,
            effects,
            was_viewer,
            was_subscribed,
            holder: s.leases().holder(core),
        })
    })
}

/// What [`subscribe_attach_terminal`] registered, and what its declared role
/// changed (ADR-0127).
pub(crate) struct AttachSubscription {
    /// The subscribed Terminal.
    pub(crate) core: phux_core::ids::ResourceId,
    /// Its engine handle.
    pub(crate) handle: ResourceHandle,
    /// What the role changed; empty when no role was applied.
    pub(crate) effects: RoleEffects,
    /// Whether the connection had declared `VIEWER` on this Terminal
    /// before this attach, so a failed widening can put the tombstone back.
    pub(crate) was_viewer: bool,
    /// Whether this command created the subscription before admission failed.
    pub(crate) was_subscribed: bool,
    /// The lease holder once the role applied, which every broadcast names.
    pub(crate) holder: Option<ClientId>,
}

/// Broadcast what an attach's declared role changed (ADR-0127) through the
/// pane's engine: `ROLE_CHANGED` first, then the lease transition.
pub(crate) async fn announce_role_effects(subscription: &AttachSubscription, client_id: ClientId) {
    announce_role_effects_on(
        &subscription.handle,
        client_id,
        subscription.effects,
        subscription.holder,
    )
    .await;
}

/// [`announce_role_effects`] for a caller holding the parts rather than an
/// [`AttachSubscription`] (the session `ATTACH` sweep).
pub(crate) async fn announce_role_effects_on(
    handle: &ResourceHandle,
    client_id: ClientId,
    effects: RoleEffects,
    holder: Option<ClientId>,
) {
    let changed = effects.role_changed.then_some(ControlAction::RoleChanged);
    for action in [changed, effects.lease].into_iter().flatten() {
        if let Some(lease) = handle.companion::<crate::resource::companions::Leasable>() {
            lease
                .lease_changed(crate::resource::LeaseChange {
                    input_holder: holder.map(super::super::wire_client),
                    action,
                    actor: Some(super::super::wire_client(client_id)),
                })
                .await;
        }
    }
}

/// The QUIC multi-stream `ATTACH_RESOURCE`: subscribe and apply the role
/// against the control mailbox, announce what the role changed, and stop.
pub(super) async fn subscribe_deferred_attach(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    role: RolePolicy,
) -> CommandResult {
    match subscribe_attach_terminal(state, client_id, terminal_id, out_tx, Some(role)) {
        Ok(subscription) => {
            announce_role_effects(&subscription, client_id).await;
            CommandResult::Ok
        }
        Err(refusal) => refusal,
    }
}

pub(super) fn invalid_agent_cursor() -> CommandResult {
    CommandResult::error(
        ErrorCode::InvalidCommand,
        "an agent resume cursor is valid only for AgentSession resources",
    )
}

/// `{ VIEWER, DELIBERATE }` on `ATTACH_RESOURCE` is `INVALID_COMMAND`
///: a viewer cannot take the wheel.
pub(super) fn refuse_invalid_role_policy(command: &Command) -> Option<CommandResult> {
    let Command::AttachResource {
        role_policy: Some(policy),
        ..
    } = command
    else {
        return None;
    };
    (!policy.is_valid()).then(|| CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: "role_policy { VIEWER, DELIBERATE } is invalid: a viewer cannot take over"
            .to_owned(),
    })
}

/// Whether a negotiated bootstrap profile streams the official progressive
/// libghostty snapshot rather than a synthesized VT bootstrap.
pub(in crate::runtime) const fn native_checkpoint_profile(
    profile: phux_protocol::caps::BootstrapStreamProfile,
) -> bool {
    matches!(
        profile,
        phux_protocol::caps::BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1
        }
    )
}
