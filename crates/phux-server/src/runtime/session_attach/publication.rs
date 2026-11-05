//! The host's one ordered aggregate publication.

use super::{ClientId, FrameKind, Outbound, SharedState};

pub(super) struct AttachPublication<'a> {
    pub(super) state: &'a SharedState,
    pub(super) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(super) client_id: ClientId,
    pub(super) attach_id: u32,
}

impl AttachPublication<'_> {
    async fn queue(&self, frame: FrameKind) -> bool {
        if self.out_tx.send(Outbound::Frame(frame)).await.is_ok() {
            return true;
        }
        crate::runtime::connections::detach_and_release_consumer_state(self.state, self.client_id);
        false
    }

    pub(super) async fn publish(
        &self,
        snapshot: phux_protocol::wire::info::SessionSnapshot,
        initial_client_id: phux_protocol::ids::ClientId,
        frames: Vec<FrameKind>,
        session_name: &str,
    ) -> bool {
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
        self.queue(FrameKind::AttachReady {
            attach_id: self.attach_id,
        })
        .await
    }
}
