//! Subscription: extracted from the shared module.
use super::{
    AttachSubscription, ClientId, Command, CommandResult, ControlAction, ControlRequest, ErrorCode,
    Outbound, ResourceHandle, RoleEffects, RolePolicy, SharedState, subscribe_attach_terminal,
};

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
                input_holder: holder.map(super::super::super::wire_client),
                action,
                actor: Some(super::super::super::wire_client(client_id)),
            })
            .await;
    }
}

/// The QUIC multi-stream `ATTACH_RESOURCE`: subscribe and apply the role
/// against the control mailbox, announce what the role changed, and stop.
pub(in crate::runtime::commands) async fn subscribe_deferred_attach(
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

pub(in crate::runtime::commands) fn invalid_agent_cursor() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: "an agent resume cursor is valid only for AgentSession resources".to_owned(),
    }
}

/// `{ VIEWER, DELIBERATE }` on `ATTACH_RESOURCE` is `INVALID_COMMAND`
/// (L1 §8.1): a viewer cannot take the wheel.
pub(in crate::runtime::commands) fn refuse_invalid_role_policy(
    command: &Command,
) -> Option<CommandResult> {
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
pub(in crate::runtime::commands) const fn native_checkpoint_profile(
    profile: phux_protocol::caps::BootstrapStreamProfile,
) -> bool {
    matches!(
        profile,
        phux_protocol::caps::BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1
        }
    )
}
