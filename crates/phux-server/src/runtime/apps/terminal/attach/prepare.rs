//! `ATTACH` preparation: session role, prior consumers, and staging.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

/// Is this ATTACH a replacement for the same client's existing attachment to
/// the same session?
pub(super) fn is_same_session_reattach(
    state: &SharedState,
    client_id: ClientId,
    session: phux_core::ids::SessionId,
) -> bool {
    state.with(|server| {
        server
            .attached()
            .get(&client_id)
            .is_some_and(|attached| attached.session == session)
    })
}

/// Apply a session `ATTACH`'s declared role (ADR-0127) to every Terminal it
/// returned in one critical section, then announce what changed.
pub(super) async fn apply_session_role(
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
            .map(|pane| {
                let effects = s.apply_attach_role(
                    client_id,
                    &pane.wire_terminal_id,
                    Some(pane.terminal_id),
                    role,
                    was_subscribed,
                );
                (effects, s.input_lease_holder(pane.terminal_id))
            })
            .collect()
    });
    for (pane, (effects, holder)) in panes.iter().zip(changes) {
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

/// Run [`prepare_attach`], translating each refusal into an `ERROR` frame.
pub(super) async fn prepare_attach_or_refuse(
    state: &SharedState,
    client_id: ClientId,
    session: phux_core::ids::SessionId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    negotiated_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Option<AttachPrepared> {
    use crate::state::AttachError;
    let error = match prepare_attach(
        state,
        client_id,
        session,
        out_tx,
        client_caps,
        negotiated_profile,
        bootstrap_limits,
    ) {
        Ok(prepared) => return Some(prepared),
        Err(error) => error,
    };
    let (code, message) = match error {
        AttachError::UnknownSession(name) => (
            ErrorCode::SessionNotFound,
            format!("session {name:?} not found"),
        ),
        AttachError::AlreadyAttached(_) => (
            ErrorCode::AlreadyAttached,
            "client is already attached".to_owned(),
        ),
        AttachError::ResourceLimit => (
            ErrorCode::CodecUnavailable,
            "session exceeds bounded aggregate attach limits".to_owned(),
        ),
    };
    send_error(out_tx, code, &message).await;
    None
}

/// Stop a state-sync consumer's prior actor-side emitters before the new
/// ATTACHED, so an old delta cannot land between it and the new bootstrap.
pub(super) async fn detach_prior_state_sync_consumers(
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
pub(super) async fn apply_client_default_colors(
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
    /// Handles the rollback boundary detaches.
    pub(super) handles: Vec<ResourceHandle>,
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
            handles: Vec::new(),
            gates: Vec::new(),
            pumps: JoinSet::new(),
        }
    }
}

impl AttachStaging {
    /// Append authoritative closures under the same frame ceiling as bootstraps.
    pub(super) fn append_closures(
        &mut self,
        terminal_ids: Vec<phux_protocol::ids::ResourceId>,
    ) -> Result<(), ()> {
        let mut frames = Vec::new();
        frames.try_reserve(terminal_ids.len()).map_err(|_| ())?;
        frames.extend(
            terminal_ids
                .into_iter()
                // Their exit watcher already consumed the close reason.
                .map(|terminal_id| FrameKind::ResourceClosed {
                    terminal_id,
                    exit_status: None,
                    reason: phux_protocol::wire::frame::CloseReason::Unknown, signal: None,
                }),
        );
        self.budget
            .append_accounted(&mut self.frames, &mut frames, 0)
    }

    /// Drop a pane that closed mid-capture from publication: its parked
    /// pump sees the gate dropped and exits without forwarding anything.
    /// Nothing of its bootstrap was staged, since every actor request that
    /// can find it gone precedes the staging.
    pub(super) fn forget_pane(&mut self, terminal_id: ResourceId) {
        self.gates.retain(|gate| gate.terminal_id != terminal_id);
    }
}

/// Why one pane's capture did not stage a bootstrap.
#[derive(Debug)]
pub(super) enum CaptureFailure {
    /// The pane's actor was gone: a request found its mailbox closed or its
    /// reply dropped. A pane only loses its actor by closing.
    Closed(String),
    /// The pane is live and its capture failed: the attach's rollback reason.
    Failed(String),
}

impl From<String> for CaptureFailure {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

/// Outcome of the ADR-0018 per-consumer state-sync registration for one pane.
#[derive(Debug, Default)]
pub(super) struct ConsumerRegistration {
    /// The actor accepted the registration.
    pub(super) registered: bool,
    /// The actor's tick is this consumer's sole live emitter: no pump.
    pub(super) tick_managed: bool,
    /// Atomic synthesized bootstrap captured in the same actor turn.
    pub(super) state_sync_bootstrap: Option<phux_terminal::actor::StateSyncBootstrap>,
    /// The actor was gone: its mailbox was closed or it dropped the reply.
    pub(super) actor_gone: bool,
}
