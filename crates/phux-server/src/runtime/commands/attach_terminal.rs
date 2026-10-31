//! Terminal attach, bootstrap, and subscription.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// Handle `ATTACH_RESOURCE` (SPEC §5.1): subscribe the caller to one
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
    announce_role_effects(&subscription, client_id).await;
    let AttachSubscription {
        core,
        handle,
        effects,
        was_viewer,
        ..
    } = subscription;

    let stream_id = crate::runtime::attach::stream_id_from(client_id.0);
    match bootstrap_attach_terminal(
        state,
        client_id,
        terminal_id,
        core,
        &handle,
        out_tx,
        stream_id,
        ctx.client_caps,
        ctx.profile,
        ctx.limits,
        &ctx.token,
        agent_after_seq,
    )
    .await
    {
        Ok(()) => CommandResult::Ok,
        Err(failure) => {
            // The COMMAND path drops its subscription on failure and undoes
            // any lease seizure or role widening it made (ADR-0127).
            let restore = was_viewer && !role.is_viewer();
            let (released, holder) = state.with_mut(|s| {
                s.unsubscribe_terminal(client_id, core);
                if restore {
                    s.set_viewer_mark(client_id, terminal_id, true);
                }
                let released = s.undo_attach_takeover(client_id, core, effects);
                (released, s.input_lease_holder(core))
            });
            if restore && effects.role_changed {
                let _ = handle
                    .control
                    .send(ControlRequest::LeaseChanged {
                        input_holder: holder.map(super::super::wire_client),
                        action: ControlAction::RoleChanged,
                        actor: Some(super::super::wire_client(client_id)),
                    })
                    .await;
            }
            if released {
                let _ = handle
                    .control
                    .send(ControlRequest::LeaseChanged {
                        input_holder: None,
                        action: ControlAction::Released,
                        actor: Some(super::super::wire_client(client_id)),
                    })
                    .await;
            }
            CommandResult::Error {
                code: failure.code,
                message: failure.message,
            }
        }
    }
}

/// Bootstrap one Terminal's content stream into `content_tx` without
/// touching the caller's subscription. The QUIC `STREAM_BIND` path
/// (docs/protocol.md#quic-stream-routing) subscribes on the control mailbox and bootstraps on the
/// stream's; the COMMAND path uses one mailbox for both.
///
/// Failures cancel the generation but never unsubscribe; the returned
/// [`AttachResourceFailure`] shapes the caller's refusal.
#[allow(
    clippy::too_many_arguments,
    reason = "negotiated connection context plus the split mailboxes"
)]
pub(crate) async fn bootstrap_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    core: phux_core::ids::ResourceId,
    handle: &ResourceHandle,
    content_tx: &tokio::sync::mpsc::Sender<Outbound>,
    stream_id: phux_protocol::ids::StreamId,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    connection_token: &CancellationToken,
    agent_after_seq: Option<u64>,
) -> Result<(), AttachResourceFailure> {
    let stream = crate::runtime::apps::StreamBootstrap {
        state,
        client_id,
        wire: terminal_id,
        core,
        handle,
        content_tx,
        stream_id,
        client_caps,
        profile: bootstrap_profile,
        limits: bootstrap_limits,
        connection_token,
        after_seq: agent_after_seq,
    };
    let Some(app) = crate::runtime::apps::app_for(handle.kind) else {
        return Err(AttachResourceFailure {
            code: ErrorCode::WrongResourceKind,
            message: format!("this server does not serve {}", handle.kind),
        });
    };
    app.bootstrap(stream).await
}

/// Resolve the wire terminal id and register the caller as an output
/// subscriber in one critical section.
///
/// The mailbox is remembered with the subscription because a session-less
/// subscriber has no other route for lifecycle fanout such as
/// `RESOURCE_CLOSED` (L1 §3.1). `role` (ADR-0127) applies in the same
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
        let core = s.terminal_from_wire(terminal_id).ok_or_else(not_found)?;
        let handle = s.resource_handle(core).cloned().ok_or_else(not_found)?;
        // The lease is a Terminal-facet concept (L1 §1.1), so a takeover of
        // any other kind is refused the way `ACQUIRE_INPUT` refuses it.
        if role.is_some_and(RolePolicy::takes_over)
            && let Some(Err(error)) = crate::runtime::apps::app_for(handle.kind)
                .map(crate::runtime::apps::ResourceApp::check_takeover)
        {
            return Err(wrong_resource_kind(error));
        }
        let was_subscribed = s.subscribers_for_terminal(core).contains(&client_id);
        let was_viewer = s.is_viewer(client_id, terminal_id);
        s.subscribe_terminal(client_id, core, Some(out_tx.clone()));
        let effects = role.map_or_else(RoleEffects::default, |policy| {
            s.apply_attach_role(client_id, terminal_id, Some(core), policy, was_subscribed)
        });
        Ok(AttachSubscription {
            core,
            handle,
            effects,
            was_viewer,
            holder: s.input_lease_holder(core),
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
        let _ = handle
            .control
            .send(ControlRequest::LeaseChanged {
                input_holder: holder.map(super::super::wire_client),
                action,
                actor: Some(super::super::wire_client(client_id)),
            })
            .await;
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
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: "an agent resume cursor is valid only for AgentSession resources".to_owned(),
    }
}

/// `{ VIEWER, DELIBERATE }` on `ATTACH_RESOURCE` is `INVALID_COMMAND`
/// (L1 §8.1): a viewer cannot take the wheel.
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
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
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
