//! The host's one ordered aggregate publication.

use super::{ClientId, FrameKind, Outbound, SharedState};
use tokio_util::sync::CancellationToken;

/// Fixed, payload-free publication labels for the host and its apps.
pub(in crate::runtime) fn publication_phase(phase: &'static str, outcome: &'static str) {
    // JSON formatting falls back to the current span even for a root event.
    // A field-free root keeps caller identities out of every retained format.
    let span = tracing::debug_span!(
        target: "phux_server::publication_diagnostics",
        parent: None,
        "publication_diagnostic"
    );
    let _entered = span.enter();
    tracing::debug!(target: "phux_server::publication_diagnostics", parent: &span, phase, outcome);
}

pub(super) struct AttachPublication<'a> {
    pub(super) state: &'a SharedState,
    pub(super) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(super) cancellation: &'a CancellationToken,
    pub(super) connection: &'a CancellationToken,
    pub(super) client_id: ClientId,
    pub(super) attach_id: u32,
}

impl AttachPublication<'_> {
    async fn queue(&self, frame: FrameKind) -> bool {
        let permit = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => return false,
            () = self.connection.cancelled() => return false,
            permit = self.out_tx.reserve() => permit,
        };
        let Ok(permit) = permit else {
            publication_phase("session_bootstrap_queue", "outbound_closed");
            return false;
        };
        self.state.with(|_| {
            if self.cancellation.is_cancelled() || self.connection.is_cancelled() {
                return false;
            }
            permit.send(Outbound::Frame(frame));
            true
        })
    }

    pub(super) async fn publish(
        &self,
        snapshot: phux_protocol::wire::info::SessionSnapshot,
        initial_client_id: phux_protocol::ids::ClientId,
        frames: Vec<FrameKind>,
        session_name: &str,
    ) -> bool {
        publication_phase("session_bootstrap_queue", "started");
        if !self
            .queue(FrameKind::Attached {
                attach_id: self.attach_id,
                snapshot,
                initial_client_id,
            })
            .await
        {
            return false;
        }
        crate::hooks::fire_hook(
            self.state,
            crate::hooks::HookEvent::client_attached(self.client_id, session_name),
        );
        for frame in frames {
            if !self.queue(frame).await {
                return false;
            }
        }
        let queued = self
            .queue(FrameKind::AttachReady {
                attach_id: self.attach_id,
            })
            .await;
        if queued {
            publication_phase("session_bootstrap_queue", "queued");
        }
        queued
    }
}
