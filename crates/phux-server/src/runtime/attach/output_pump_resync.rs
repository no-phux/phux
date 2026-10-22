//! Output pump generation republication and gap resync.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

impl OutputPumpContext {
    /// Tombstone the live generation, publish its native replacement, and
    /// adopt the post-cut receiver the actor fenced it behind. `false` means
    /// the actor invalidated the capture: the generation stays retired.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn republish_native_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        prior_bootstrap_id: BootstrapId,
        reason: crate::terminal_actor::ResyncReason,
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
        // The id advances only once the replacement frames are queued.
        let bootstrap_id = next_bootstrap_id(prior_bootstrap_id);
        let Some(reply) = self.capture_native_checkpoint(bootstrap_id).await? else {
            generation.retire();
            return Ok(false);
        };
        self.publish_captured_native_generation(generation, output_rx, bootstrap_id, reply)
            .await
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn publish_captured_native_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        bootstrap_id: BootstrapId,
        reply: crate::terminal_actor::NativeBootstrapReply,
    ) -> Result<bool, PumpFault> {
        let (cut, cursor) = publish_native_bootstrap(&self.out_tx, reply)
            .await
            .map_err(|()| PumpFault::GenerationLost)?;
        generation.set_bootstrap_id(bootstrap_id);
        let publication = activate_native_publication(
            &self.terminal,
            self.client_id.0,
            self.wire_terminal_id.clone(),
            self.stream_id,
            bootstrap_id,
            cursor,
        )
        .await
        .map_err(|failure| {
            // The pane exited while this pump was blocked publishing its
            // replacement to a slow consumer: the pane ended, the client
            // did not.
            if failure.actor_gone() {
                PumpFault::PaneGone
            } else {
                PumpFault::PublicationNotActivated
            }
        })?;
        // Unfence before the replay so it passes the same `forwards` gate as
        // live output: a replay entry at or behind the cut is already in the
        // checkpoint, and resending it is a `DuplicateSequence` the client
        // detaches on.
        generation.republished_at(cut);
        *output_rx = publication.live;
        self.forward_gated_replay(generation, publication.replay)
            .await?;
        self.publish_last_seq(generation);
        // Chunks queued behind the replay waited on it, not on the consumer.
        generation.restart_staleness_clock();
        Ok(true)
    }

    /// Capture after EOF/retention expiry, independently of earlier Exit
    /// broadcasts. Only capture has a deadline: cancelling a publication after
    /// BEGIN could leave the client with a partial generation.
    pub(super) async fn publish_final_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
    ) -> Result<(), PumpFault> {
        const CAPTURE_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if publishes_native_checkpoints(self.profile) {
            let bootstrap_id = next_bootstrap_id(generation.bootstrap_id());
            let capture = request_native_checkpoint(&self.terminal, |reply| {
                crate::terminal_actor::NativeBootstrapRequest::full_budget(
                    self.client_id.0,
                    self.wire_terminal_id.clone(),
                    self.stream_id,
                    bootstrap_id,
                    self.limits,
                    reply,
                )
            });
            let reply = tokio::time::timeout(CAPTURE_WAIT, capture)
                .await
                .map_err(|_| PumpFault::PublicationNotActivated)?
                .map_err(|failure| match failure {
                    NativeRequestFailure::Unsent | NativeRequestFailure::Dropped => {
                        PumpFault::PaneGone
                    }
                    NativeRequestFailure::Refused(_) => PumpFault::PublicationNotActivated,
                })?;
            if generation.is_active() {
                self.out_tx
                    .send(Outbound::Frame(self.tombstone_frame(
                        generation,
                        crate::terminal_actor::ResyncReason::Exit,
                    )))
                    .await
                    .map_err(|_| PumpFault::TombstoneNotQueued)?;
            }
            self.publish_captured_native_generation(generation, output_rx, bootstrap_id, reply)
                .await?;
            return Ok(());
        }
        let _ = output_rx;
        let snapshot = self
            .snapshot
            .as_ref()
            .ok_or(PumpFault::PublicationNotActivated)?;
        let capture = async {
            let (reply, receive) = oneshot::channel();
            snapshot
                .send(SnapshotRequest {
                    scrollback: None,
                    max_bytes: bootstrap_source_ceiling(
                        MAX_STAGED_BOOTSTRAP_BYTES,
                        self.client_caps,
                    ),
                    max_frames: MAX_STAGED_BOOTSTRAP_FRAMES - 1,
                    chunk_bytes: self.limits.max_chunk_bytes() as usize,
                    reply,
                })
                .await
                .map_err(|_| PumpFault::PaneGone)?;
            receive
                .await
                .map_err(|_| PumpFault::PaneGone)?
                .map_err(|_| PumpFault::PublicationNotActivated)
        };
        let (snapshot, base_seq) = tokio::time::timeout(CAPTURE_WAIT, capture)
            .await
            .map_err(|_| PumpFault::PublicationNotActivated)??;
        let resync = PaneResync {
            cols: snapshot.cols,
            rows: snapshot.rows,
            bytes: snapshot.bytes.into(),
            base_seq,
            reason: crate::terminal_actor::ResyncReason::Exit,
        };
        match self
            .republish_synthesized_generation(generation, &resync)
            .await
        {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(fault) => Err(fault.unwrap_or(PumpFault::PublicationNotActivated)),
        }
    }

    /// `BOOTSTRAP_TOMBSTONE` retiring the generation this pump publishes.
    fn tombstone_frame(
        &self,
        generation: &PumpGeneration,
        reason: crate::terminal_actor::ResyncReason,
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
    /// live generation is tombstoned first (L1 §4.6), in the same queued
    /// batch, so a deferred resync retires nothing.
    async fn republish_synthesized_generation(
        &self,
        generation: &mut PumpGeneration,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        let payload = downsample_for_caps(&resync.bytes, self.client_caps);
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
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
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
        self.republish_synthesized_generation(generation, resync)
            .await
    }

    /// The actor invalidated this pump's capture and the generation is
    /// already tombstoned. Stay fenced and ask for the resync, under the gap
    /// retry budget, so a lost broadcast cannot freeze the client; a closing
    /// pane ends the pump quietly instead.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn await_superseded_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        if generation.fence_for_gap() {
            return ControlFlow::Continue(());
        }
        self.send_gap_resync(generation).await
    }

    /// A dropped broadcast window leaves the client's mirror stale: fence the
    /// generation (before the request, even if it fails, so gapped frames are
    /// never forwarded), ask for an in-band resync, and lose the generation if
    /// none can be delivered.
    pub(super) async fn request_gap_resync(
        &self,
        generation: &mut PumpGeneration,
        cause: GapCause,
    ) -> ControlFlow<Option<PumpFault>> {
        crate::perf::PUMP_LAGGED.incr();
        if generation.fence_for_gap() {
            debug!(
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} lagged again while a resync was already in flight; waiting",
                self.lag_label,
            );
            return ControlFlow::Continue(());
        } else if matches!(cause, GapCause::Stale(_)) {
            // A slow link goes stale every cycle by design; `pump.lagged`
            // counts it.
            debug!(
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} fell behind; skipping to a fresh checkpoint",
                self.lag_label,
            );
        } else {
            warn!(
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} lagged; requesting in-band resync",
                self.lag_label,
            );
        }
        crate::perf::PUMP_GAP_RESYNC.incr();
        self.send_gap_resync(generation).await
    }

    /// Ask the actor for this pump's resync. A gone actor ends the pump
    /// quietly (`RESOURCE_CLOSED` follows); a stuck one loses the generation.
    async fn send_gap_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        generation.note_resync_requested();
        if enqueue_output_resync(&self.resize, self.resync_target(generation)).await {
            return ControlFlow::Continue(());
        }
        if self.resize.is_closed() {
            return ControlFlow::Break(None);
        }
        self.fail_unrecoverable_gap().await
    }

    /// The resync asked for at the last gap has not arrived within
    /// [`pump::GAP_RESYNC_RETRY`]: ask again, or the fenced client would sit
    /// on a screen that never changes.
    pub(super) async fn retry_gap_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        debug!(
            terminal_id = ?self.wire_terminal_id,
            attempt = generation.gap_attempts(),
            "{} is still waiting on its in-band resync; re-requesting",
            self.lag_label,
        );
        self.send_gap_resync(generation).await
    }

    /// The gap spent its whole request budget without the actor ever
    /// broadcasting a replacement generation.
    pub(super) async fn abandon_unanswered_gap(
        &self,
        generation: &PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        warn!(
            terminal_id = ?self.wire_terminal_id,
            attempts = generation.gap_attempts(),
            "{} never received the in-band resync it asked for; failing the generation",
            self.lag_label,
        );
        self.fail_unrecoverable_gap().await
    }

    /// Tell the client the gap is unrecoverable and end the pump.
    async fn fail_unrecoverable_gap(&self) -> ControlFlow<Option<PumpFault>> {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.out_tx.send(Outbound::Frame(FrameKind::Error {
                request_id: None,
                code: ErrorCode::InternalError,
                message: "terminal output gap could not be resynchronized".to_owned(),
            })),
        )
        .await;
        ControlFlow::Break(Some(PumpFault::GenerationLost))
    }
}
