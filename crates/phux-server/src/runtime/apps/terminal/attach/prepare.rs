//! `ATTACH` preparation: session role, prior consumers, and staging.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal_api::actor::TerminalResource as _;

/// Apply a session `ATTACH`'s declared role (ADR-0127) to every Terminal it
/// returned in one critical section, then announce what changed.
pub(crate) async fn apply_session_role(
    state: &SharedState,
    client_id: ClientId,
    panes: &[crate::state::AttachSnapshotPane],
    role: phux_protocol::wire::frame::RolePolicy,
    was_subscribed: bool,
) {
    let changes: Vec<_> = state.with_mut(|s| {
        s.set_attached_viewer(client_id, role.is_viewer());
        panes
            .iter()
            .filter(|pane| pane.handle.kind == crate::resource::ResourceKind::Terminal)
            .map(|pane| {
                let effects = s.apply_attach_role(
                    client_id,
                    &pane.wire_terminal_id,
                    Some(pane.terminal_id),
                    role,
                    was_subscribed,
                );
                (pane, effects, s.input_lease_holder(pane.terminal_id))
            })
            .collect()
    });
    for (pane, effects, holder) in changes {
        if !effects.is_empty() {
            crate::runtime::commands::announce_role_effects_on(
                &pane.handle,
                client_id,
                effects,
                holder,
            )
            .await;
        }
    }
}

/// Stop a state-sync consumer's prior actor-side emitters before the new
/// ATTACHED, so an old delta cannot land between it and the new bootstrap.
pub(crate) async fn detach_prior_state_sync_consumers(
    panes: &[AttachSnapshotPane],
    wire_client_id: phux_protocol::ids::ClientId,
    client_caps: ClientCapabilities,
) {
    if !matches!(
        client_caps.output_mode,
        phux_protocol::caps::OutputMode::StateSync
    ) {
        return;
    }
    for pane in panes {
        let Ok(terminal) = pane.handle.terminal() else {
            continue;
        };
        let (reply, done) = oneshot::channel();
        if terminal
            .consumer_detach
            .send(ConsumerDetachRequest {
                client_id: wire_client_id,
                reply,
            })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
}

/// The most recent attach that advertises a palette sets the panes' default
/// colors; each is acknowledged before snapshotting so later OSC 10/11
/// queries see it.
pub(crate) async fn apply_client_default_colors(
    panes: &[AttachSnapshotPane],
    colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) {
    let Some(colors) = colors else {
        return;
    };
    for pane in panes {
        let Ok(terminal) = pane.handle.terminal() else {
            continue;
        };
        let (reply, done) = oneshot::channel();
        if terminal
            .set_default_colors
            .send(SetDefaultColorsRequest { colors, reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
}

/// The staging state one ATTACH accumulates, one pane at a time: each
/// capture is charged before the next actor gets its remaining ceiling, so
/// concurrent allocations never exceed the connection-wide cap.
pub(super) struct AttachStaging {
    pub(super) budget: BootstrapStagingBudget,
    /// Bootstrap and closure frames staged for atomic publication.
    pub(super) frames: Vec<FrameKind>,
    /// Per-pane publication gates.
    pub(super) gates: Vec<SnapshotGate>,
    pub(super) pumps: JoinSet<()>,
}

impl Default for AttachStaging {
    fn default() -> Self {
        Self {
            budget: BootstrapStagingBudget::with_limits(
                MAX_STAGED_BOOTSTRAP_BYTES,
                MAX_STAGED_BOOTSTRAP_FRAMES,
            ),
            frames: Vec::new(),
            gates: Vec::new(),
            pumps: JoinSet::new(),
        }
    }
}

/// Why one pane's capture did not stage a bootstrap.
pub(crate) use crate::runtime::session_attach::staging::CaptureFailure;

/// Outcome of the ADR-0018 per-consumer state-sync registration for one pane.
#[derive(Debug, Default)]
pub(super) struct ConsumerRegistration {
    /// The actor accepted the registration.
    pub(super) registered: bool,
    /// The actor's tick is this consumer's sole live emitter: no pump.
    pub(super) tick_managed: bool,
    /// Atomic synthesized bootstrap captured in the same actor turn.
    pub(super) state_sync_bootstrap: Option<phux_terminal_api::actor::StateSyncBootstrap>,
    /// The actor was gone: its mailbox was closed or it dropped the reply.
    pub(super) actor_gone: bool,
}
