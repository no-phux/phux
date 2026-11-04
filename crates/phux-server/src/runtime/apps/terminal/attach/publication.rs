//! The atomic `ATTACH` publication.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

/// The atomic ATTACH publication, in the one order the handshake permits.
pub(super) struct AttachPublication<'a> {
    pub(in crate::runtime) state: &'a SharedState,
    pub(in crate::runtime) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// Cancelled when a published generation cannot be activated.
    pub(super) connection_token: &'a CancellationToken,
    pub(in crate::runtime) client_id: ClientId,
    pub(super) attach_id: u32,
    pub(super) stream_id: StreamId,
    pub(super) bootstrap_id: BootstrapId,
}

impl AttachPublication<'_> {
    /// Queue one publication frame, releasing this client's consumer state
    /// when the outbound mailbox has closed.
    pub(super) async fn queue(&self, frame: FrameKind) -> bool {
        if self.out_tx.send(Outbound::Frame(frame)).await.is_ok() {
            return true;
        }
        crate::runtime::connections::detach_and_release_consumer_state(self.state, self.client_id);
        false
    }

    /// Queue `ATTACHED`, every staged pane bootstrap or authoritative closure,
    /// then `ATTACH_READY`. `false` once the client's mailbox has closed and
    /// the attach is abandoned.
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

    /// Release every parked output pump now that the publication is on the
    /// wire, activating native publications for their replay and post-cut
    /// receiver.
    pub(super) async fn release_gates(&self, gates: Vec<SnapshotGate>) {
        for gate in gates {
            let Some(cut) = gate.cut else {
                continue;
            };
            let mut start = OutputPumpStart {
                published_cut: cut,
                replay: Vec::new(),
                live: None,
            };
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            if let Some(cursor) = gate.native_cursor {
                let publication = match activate_native_publication(
                    &gate.terminal,
                    self.client_id.0,
                    gate.wire_terminal_id,
                    self.stream_id,
                    self.bootstrap_id,
                    cursor,
                )
                .await
                {
                    Ok(publication) => publication,
                    // The pane exited after its capture: dropping the gate
                    // ends its pump quietly, and `RESOURCE_CLOSED` tells the
                    // client. The rest of the attach is unaffected.
                    Err(failure) if failure.actor_gone() => continue,
                    Err(_) => {
                        crate::runtime::connections::detach_and_release_consumer_state(
                            self.state,
                            self.client_id,
                        );
                        self.connection_token.cancel();
                        return;
                    }
                };
                start.replay = publication.replay;
                start.live = Some(publication.live);
            }
            let _ = gate.gate.send(start);
        }
    }
}
