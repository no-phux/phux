//! Output pump generation republication.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

#[cfg(test)]
#[path = "tests_image_resync.rs"]
mod tests;

/// The tracing target of the stale-output line. Tools filter on it
/// (`tools/src/web/browser.ts`), so it stays put when this module moves. It
/// keeps the crate prefix so a `phux_server=debug` filter still shows it.
pub(super) const LOG_TARGET: &str = "phux_server::output_resync";
const FINAL_CAPTURE_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

impl OutputPumpContext {
    /// Tombstone the live generation, publish its native replacement, and
    /// adopt the post-cut receiver the actor fenced it behind. `false` means
    /// the actor invalidated the capture: the generation stays retired.
    async fn republish_native_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
        prior_bootstrap_id: BootstrapId,
        reason: crate::resource::ResyncReason,
    ) -> Result<bool, PumpFault> {
        if generation.is_active()
            && self
                .out_tx
                .send(Outbound::Frame(self.tombstone_frame(generation, reason)))
                .await
                .is_err()
        {
            return Err(PumpFault::TombstoneNotQueued);
        }
        generation.retire();
        // The id advances only once the replacement frames are queued.
        let bootstrap_id = next_bootstrap_id(prior_bootstrap_id);
        let Some(reply) = self.capture_native_checkpoint(bootstrap_id).await? else {
            generation.retire();
            return Ok(false);
        };
        self.publish_captured_native_generation(generation, output_rx, bootstrap_id, reply)
            .await
    }

    async fn publish_captured_native_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
        bootstrap_id: BootstrapId,
        reply: phux_terminal_api::actor::NativeBootstrapReply,
    ) -> Result<bool, PumpFault> {
        let (cut, capture) = publish_native_bootstrap(&self.out_tx, reply)
            .await
            .map_err(|()| PumpFault::GenerationLost)?;
        generation.set_bootstrap_id(bootstrap_id);
        let publication = activate_native_publication(
            &self.terminal,
            capture,
        )
        .await
        .map_err(|failure| {
            // The pane exited while this pump was blocked publishing its
            // replacement to a slow consumer: the pane ended, the client
            // did not.
            if failure.actor_gone() {
                super::bootstrap::publication_phase("replacement_generation", "pane_gone");
                PumpFault::PaneGone
            } else {
                super::bootstrap::publication_phase("replacement_generation", "activation_refused");
                PumpFault::PublicationNotActivated
            }
        })?;
        let NativePublication::Activated(mut publication) = publication else {
            self.retire_superseded_publication(generation, cut).await?;
            return Ok(false);
        };
        // Unfence before the replay so it passes the same `forwards` gate as
        // live output: a replay entry at or behind the cut is already in the
        // checkpoint, and resending it is a `DuplicateSequence` the client
        // detaches on.
        generation.republished_at(cut);
        *output_rx = publication.live;
        self.forward_gated_replay(generation, publication.replay)
            .await?;
        publication.interest.published();
        self.publish_last_seq(generation);
        // Chunks queued behind the replay waited on it, not on the consumer.
        generation.restart_staleness_clock();
        Ok(true)
    }

    /// READY is queued but never activated: its only valid sequence is the
    /// checkpoint cut, not any output the actor queued while activation waited.
    pub(super) async fn retire_superseded_publication(
        &self,
        generation: &mut PumpGeneration,
        cut: u64,
    ) -> Result<(), PumpFault> {
        self.out_tx
            .send(Outbound::Frame(FrameKind::BootstrapTombstone {
                terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id: generation.bootstrap_id(),
                reason: phux_protocol::wire::frame::TombstoneReason::Resize,
                last_valid_seq: cut,
            }))
            .await
            .map_err(|_| PumpFault::TombstoneNotQueued)?;
        generation.retire();
        Ok(())
    }

    /// Capture after EOF/retention expiry, independently of earlier Exit
    /// broadcasts. Only capture has a deadline: cancelling a publication after
    /// BEGIN could leave the client with a partial generation.
    pub(super) async fn publish_final_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
    ) -> Result<bool, PumpFault> {
        if publishes_native_checkpoints(self.profile) {
            return self
                .publish_final_native_generation(generation, output_rx)
                .await;
        }
        let _ = output_rx;
        let snapshot = self.snapshot.as_ref().ok_or_else(|| {
            super::bootstrap::publication_phase("final_synthesized_capture", "unavailable");
            PumpFault::PublicationNotActivated
        })?;
        let capture = async {
            let (reply, receive) = oneshot::channel();
            snapshot
                .send(SnapshotRequest {
                    scrollback: None,
                    image_protocols: self.client_caps.image_protocols,
                    max_bytes: bootstrap_source_ceiling(
                        &self.terminal.presentation,
                        MAX_STAGED_BOOTSTRAP_BYTES,
                        self.client_caps,
                    ),
                    max_frames: MAX_STAGED_BOOTSTRAP_FRAMES - 1,
                    chunk_bytes: self.limits.max_chunk_bytes() as usize,
                    reply,
                })
                .await
                .map_err(|_| {
                    super::bootstrap::publication_phase(
                        "final_synthesized_capture",
                        "actor_unsent",
                    );
                    PumpFault::PaneGone
                })?;
            receive
                .await
                .map_err(|_| {
                    super::bootstrap::publication_phase(
                        "final_synthesized_capture",
                        "actor_dropped",
                    );
                    PumpFault::PaneGone
                })?
                .map_err(|_| {
                    super::bootstrap::publication_phase("final_synthesized_capture", "refused");
                    PumpFault::PublicationNotActivated
                })
        };
        let (snapshot, base_seq) = tokio::time::timeout(FINAL_CAPTURE_WAIT, capture)
            .await
            .map_err(|_| {
                super::bootstrap::publication_phase("final_synthesized_capture", "deadline");
                PumpFault::PublicationNotActivated
            })??;
        let resync = PaneResync {
            cols: snapshot.cols,
            rows: snapshot.rows,
            bytes: snapshot.bytes.into(),
            base_seq,
            reason: crate::resource::ResyncReason::Exit,
        };
        match self
            .republish_synthesized_generation(generation, &resync)
            .await
        {
            ControlFlow::Continue(()) => Ok(generation.is_active() && !generation.is_fenced()),
            ControlFlow::Break(fault) => Err(fault.unwrap_or(PumpFault::PublicationNotActivated)),
        }
    }

    async fn publish_final_native_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
    ) -> Result<bool, PumpFault> {
        let bootstrap_id = next_bootstrap_id(generation.bootstrap_id());
        let capture = request_native_checkpoint(&self.terminal, |reply| {
            phux_terminal_api::actor::NativeBootstrapRequest::full_budget(
                self.client_id.0,
                self.wire_terminal_id.clone(),
                self.stream_id,
                bootstrap_id,
                self.limits,
                reply,
            )
        });
        let reply = tokio::time::timeout(FINAL_CAPTURE_WAIT, capture)
            .await
            .map_err(|_| {
                super::bootstrap::publication_phase("final_native_capture", "deadline");
                PumpFault::PublicationNotActivated
            })?
            .map_err(|failure| match failure {
                NativeRequestFailure::Unsent | NativeRequestFailure::Dropped => {
                    super::bootstrap::publication_phase("final_native_capture", "pane_gone");
                    PumpFault::PaneGone
                }
                NativeRequestFailure::Refused(_) => {
                    super::bootstrap::publication_phase("final_native_capture", "refused");
                    PumpFault::PublicationNotActivated
                }
            })?;
        if generation.is_active() {
            self.out_tx
                .send(Outbound::Frame(self.tombstone_frame(
                    generation,
                    crate::resource::ResyncReason::Exit,
                )))
                .await
                .map_err(|_| PumpFault::TombstoneNotQueued)?;
        }
        generation.retire();
        self.publish_captured_native_generation(generation, output_rx, bootstrap_id, reply)
            .await
    }

    /// `BOOTSTRAP_TOMBSTONE` retiring the generation this pump publishes.
    fn tombstone_frame(
        &self,
        generation: &PumpGeneration,
        reason: crate::resource::ResyncReason,
    ) -> FrameKind {
        FrameKind::BootstrapTombstone {
            terminal_id: self.wire_terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: generation.bootstrap_id(),
            reason: tombstone_reason_for(reason),
            last_valid_seq: generation.last_forwarded_seq(),
        }
    }

    /// Publish the synthesized-VT replacement generation for a resync. A
    /// live generation is tombstoned first, in the same queued
    /// batch, so a deferred resync retires nothing.
    async fn republish_synthesized_generation(
        &self,
        generation: &mut PumpGeneration,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        let payload =
            downsample_for_caps(&self.terminal.presentation, &resync.bytes, self.client_caps);
        let bootstrap_id = next_bootstrap_id(generation.bootstrap_id());
        let Ok(mut frames) = synthesized_bootstrap_frames(
            self.wire_terminal_id.clone(),
            self.stream_id,
            bootstrap_id,
            self.profile,
            self.limits,
            resync.cols,
            resync.rows,
            resync.base_seq,
            [payload],
        ) else {
            return ControlFlow::Break(Some(PumpFault::OutboundClosed));
        };
        if generation.is_active() {
            frames.insert(0, self.tombstone_frame(generation, resync.reason));
        }
        match queue_resync_bootstrap(&self.out_tx, resync.reason, frames, generation.is_fenced())
            .await
        {
            SnapshotQueue::Queued => {
                generation.set_bootstrap_id(bootstrap_id);
                generation.republished_at(resync.base_seq);
                self.publish_last_seq(generation);
                ControlFlow::Continue(())
            }
            // Still fenced; retry or natural-close drain can replace this generation.
            SnapshotQueue::Deferred => ControlFlow::Continue(()),
            SnapshotQueue::Closed => ControlFlow::Break(Some(PumpFault::OutboundClosed)),
        }
    }

    /// Replace the published generation after the actor resynchronized the
    /// pane. Even an unchanged cut replaces it: resync is a control event.
    pub(super) async fn republish_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<ResourceOutput>,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        if publishes_native_checkpoints(self.profile) {
            let prior_bootstrap_id = generation.bootstrap_id();
            return match self
                .republish_native_generation(
                    generation,
                    output_rx,
                    prior_bootstrap_id,
                    resync.reason,
                )
                .await
            {
                Ok(true) => ControlFlow::Continue(()),
                Ok(false) => self.await_superseded_resync(generation).await,
                Err(fault) => ControlFlow::Break(Some(fault)),
            };
        }
        // The shared resync carries text only. Capture images only for pumps
        // that negotiated them, under the same bounded source budget.
        if self
            .client_caps
            .image_protocols
            .contains(phux_protocol::caps::ImageProtocol::KittyGraphics)
        {
            let Some(snapshot) = self.snapshot.as_ref() else {
                return ControlFlow::Break(Some(PumpFault::PublicationNotActivated));
            };
            let (reply, receive) = oneshot::channel();
            if snapshot
                .send(SnapshotRequest {
                    scrollback: Some(phux_terminal_api::actor::DEFAULT_REPLAY_SCROLLBACK_LINES),
                    image_protocols: self.client_caps.image_protocols,
                    max_bytes: bootstrap_source_ceiling(
                        &self.terminal.presentation,
                        MAX_STAGED_BOOTSTRAP_BYTES,
                        self.client_caps,
                    )
                    .min(32 * 1024 * 1024),
                    max_frames: MAX_STAGED_BOOTSTRAP_FRAMES - 1,
                    chunk_bytes: self.limits.max_chunk_bytes() as usize,
                    reply,
                })
                .await
                .is_err()
            {
                return ControlFlow::Break(Some(PumpFault::PaneGone));
            }
            let Ok(Ok((snapshot, base_seq))) = receive.await else {
                return ControlFlow::Break(Some(PumpFault::PublicationNotActivated));
            };
            let bytes = if snapshot.scrollback.is_empty() {
                snapshot.bytes
            } else {
                let mut bytes = snapshot.scrollback;
                bytes.reserve_exact(snapshot.bytes.len());
                bytes.extend_from_slice(&snapshot.bytes);
                bytes
            };
            let captured = PaneResync {
                cols: snapshot.cols,
                rows: snapshot.rows,
                bytes: bytes.into(),
                base_seq,
                reason: resync.reason,
            };
            return self
                .republish_synthesized_generation(generation, &captured)
                .await;
        }
        self.republish_synthesized_generation(generation, resync)
            .await
    }
}
