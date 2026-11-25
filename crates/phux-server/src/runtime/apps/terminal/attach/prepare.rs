//! `ATTACH` palette preparation and Terminal capture staging.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal_api::actor::TerminalResource as _;

/// Apply palette changes after the host has fenced exact predecessor lifetimes.
pub(crate) async fn prepare_consumers(panes: &[AttachSnapshotPane], caps: ClientCapabilities) {
    apply_client_default_colors(panes, caps.default_colors).await;
}

/// The most recent attach that advertises a palette sets the panes' default
/// colors; each is acknowledged before snapshotting so later OSC 10/11
/// queries see it.
async fn apply_client_default_colors(
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

/// A Terminal's consumer owns engine state. Report a missing facet on a
/// Terminal handle because it prevents teardown.
fn routed_terminal(handle: &ResourceHandle) -> Option<&phux_terminal_api::actor::TerminalHandle> {
    if handle.kind != crate::resource::ResourceKind::Terminal {
        return None;
    }
    handle
        .terminal()
        .inspect_err(|error| tracing::error!(%error, "consumer teardown skipped"))
        .ok()
}

/// Free a client's native cut and consumer entry without blocking: a full
/// mailbox is reaped by `tick_emit` when the client's mailbox closes.
pub(crate) fn release_consumer(handle: &ResourceHandle, client_id: ClientId) {
    let Some(terminal) = routed_terminal(handle) else {
        return;
    };
    let _ = terminal
        .native_release
        .try_send(phux_terminal_api::actor::NativeReleaseRequest { owner: client_id.0 });
    let (reply_tx, _reply_rx) = oneshot::channel();
    match terminal
        .consumer_detach
        .try_send(phux_terminal_api::actor::ConsumerDetachRequest {
            client_id: crate::runtime::wire_client(client_id),
            generation: None,
            reply: reply_tx,
        }) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            trace!(
                ?client_id,
                "consumer_detach mailbox full; entry reaped by tick_emit when its mailbox closes",
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            trace!(
                ?client_id,
                "consumer_detach: pane actor gone; nothing to free"
            );
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
    /// Pending cuts, pump gates and exact actor interest have one owner.
    pub(super) publication: super::publication::TerminalPublication,
}

impl Default for AttachStaging {
    fn default() -> Self {
        Self {
            budget: BootstrapStagingBudget::with_limits(
                MAX_STAGED_BOOTSTRAP_BYTES,
                MAX_STAGED_BOOTSTRAP_FRAMES,
            ),
            frames: Vec::new(),
            publication: super::publication::TerminalPublication::default(),
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
    pub(super) guard: Option<phux_terminal_api::actor::ConsumerRegistrationGuard>,
}
