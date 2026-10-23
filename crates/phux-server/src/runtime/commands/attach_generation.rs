//! Attach generation: extracted from the shared module.
use super::{
    AttachResourceFailure, AttachResourceGeneration, AttachResourcePumpChannels, BootstrapLimits,
    CancellationToken, ClientCapabilities, ClientId, ErrorCode, FrameKind, Outbound,
    ResourceHandle, SharedState, TerminalHandle, debug, native_checkpoint_profile, oneshot, pump,
};

/// One in-flight `ATTACH_RESOURCE`: the resolved terminal plus the negotiated
/// connection context every stage needs. Every failure funnels through
/// [`Self::failed`].
pub(super) struct AttachResourceSession<'a> {
    pub(super) state: &'a SharedState,
    pub(super) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(super) connection_token: &'a CancellationToken,
    pub(super) terminal_id: &'a phux_protocol::ids::ResourceId,
    pub(super) core: phux_core::ids::ResourceId,
    /// The resource's generic channels (output, consumers, control).
    pub(super) handle: ResourceHandle,
    /// The Terminal facet of `handle`, resolved once at entry.
    pub(super) terminal: TerminalHandle,
    pub(super) client_id: ClientId,
    pub(super) stream_id: phux_protocol::ids::StreamId,
    pub(super) client_caps: ClientCapabilities,
    pub(super) stream_profile: phux_protocol::caps::BootstrapStreamProfile,
    pub(super) bootstrap_limits: BootstrapLimits,
}

impl AttachResourceSession<'_> {
    /// Whether this connection negotiated the actor-emitted state-sync stream
    /// at HELLO.
    pub(super) const fn wants_state_sync(&self) -> bool {
        matches!(
            self.client_caps.output_mode,
            phux_protocol::caps::OutputMode::StateSync
        )
    }

    /// Undo a partial generation: cancel it, release any native lease, and
    /// detach the per-consumer state entry. The subscription is the caller's.
    pub(super) fn failed(&self, failure: AttachResourceFailure) -> AttachResourceFailure {
        use crate::terminal_actor::ConsumerDetachRequest;

        self.state.with_mut(|s| {
            s.cancel_attach_terminal_pump(self.client_id, self.core);
        });
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let _ =
            self.terminal
                .native_release
                .try_send(crate::terminal_actor::NativeReleaseRequest {
                    owner: self.client_id.0,
                });
        let (reply, _ack) = oneshot::channel();
        let _ = self.handle.consumer_detach.try_send(ConsumerDetachRequest {
            client_id: super::super::wire_client(self.client_id),
            reply,
        });
        failure
    }

    /// Register the per-consumer state-sync entry (ADR-0018) so `FRAME_ACK`
    /// drives the actor's eviction loop. `None` when the terminal is not local
    /// or the actor refused.
    pub(super) async fn register_consumer(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        live_gate: tokio::sync::watch::Receiver<bool>,
    ) -> Option<crate::terminal_actor::ConsumerAttachOutcome> {
        use crate::terminal_actor::ConsumerAttachRequest;

        let wire_terminal_id = self.terminal_id.local_id()?;
        let (reply, reply_rx) = oneshot::channel();
        self.handle
            .consumer_attach
            .send(ConsumerAttachRequest {
                client_id: super::super::wire_client(self.client_id),
                outbound: self.out_tx.clone(),
                wire_terminal_id,
                stream_id: self.stream_id,
                bootstrap_id,
                wants_state_sync: self.wants_state_sync(),
                live_gate,
                state_sync_scrollback: Some(
                    crate::resource::terminal::DEFAULT_REPLAY_SCROLLBACK_LINES,
                ),
                bootstrap_max_bytes: crate::runtime::attach::bootstrap_source_ceiling(
                    crate::runtime::attach::MAX_STAGED_BOOTSTRAP_BYTES,
                    self.client_caps,
                ),
                bootstrap_max_frames: crate::runtime::attach::MAX_STAGED_BOOTSTRAP_FRAMES,
                bootstrap_chunk_bytes: usize::try_from(self.bootstrap_limits.max_chunk_bytes())
                    .ok()?,
                // Reliable transport: emit-once is correct (ADR-0042).
                loss_tolerant: false,
                reply,
            })
            .await
            .ok()?;
        let Ok(Ok(outcome)) = reply_rx.await else {
            return None;
        };
        Some(outcome)
    }

    /// Allocate the pump generation for this attach: register the consumer,
    /// tombstone whatever generation it replaces, and start the raw output
    /// pump unless the actor's tick owns emission.
    pub(super) async fn establish_generation(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<AttachResourceGeneration, AttachResourceFailure> {
        // Subscribe before stopping the old pump so replacement never loses a
        // byte emitted in the handoff window. The new receiver remains gated
        // until this generation reaches READY.
        let output_rx = self.handle.output.subscribe();
        pump::stop_output(self.state, self.client_id, self.core).await;
        let (token, pump_done, generation_last_seq, prior) = self
            .state
            .with_mut(|s| s.replace_attach_terminal_pump(self.client_id, self.core, bootstrap_id));
        let mut pump_done_guard = Some(pump_done.drop_guard());
        if let Some((_, prior_done, _)) = &prior {
            prior_done.cancelled().await;
        }
        let (live_gate_tx, live_gate_rx) = tokio::sync::watch::channel(false);

        let outcome = self.register_consumer(bootstrap_id, live_gate_rx).await;
        if self.wants_state_sync() && outcome.is_none() {
            return Err(AttachResourceFailure::internal(
                "ATTACH_RESOURCE state-sync registration failed",
            ));
        }
        let tick_managed = outcome.as_ref().is_some_and(|outcome| outcome.tick_managed);
        if tick_managed {
            // No raw pump will own this generation.
            drop(pump_done_guard.take());
        }
        if let Some((prior_bootstrap_id, _, prior_last_seq)) = prior
            && self
                .out_tx
                .send(Outbound::Frame(FrameKind::BootstrapTombstone {
                    terminal_id: self.terminal_id.clone(),
                    stream_id: self.stream_id,
                    bootstrap_id: prior_bootstrap_id,
                    reason: phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
                    last_valid_seq: prior_last_seq.load(std::sync::atomic::Ordering::Acquire),
                }))
                .await
                .is_err()
        {
            return Err(AttachResourceFailure::internal(
                "consumer went away during ATTACH_RESOURCE replacement",
            ));
        }

        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_publication_gate_tx, native_publication_gate_rx) =
            oneshot::channel::<crate::terminal_actor::NativePublicationReply>();
        let snapshot_gate = if tick_managed {
            None
        } else {
            let Some(pump_done_guard) = pump_done_guard.take() else {
                return Err(AttachResourceFailure::internal(
                    "ATTACH_RESOURCE pump generation lost its completion guard",
                ));
            };
            let (gate_tx, gate_rx) = oneshot::channel::<u64>();
            let channels = AttachResourcePumpChannels {
                token,
                output_rx,
                snapshot_gate: gate_rx,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                native_publication_gate: native_publication_gate_rx,
            };
            let last_seq = std::sync::Arc::clone(&generation_last_seq);
            self.spawn_output_pump(channels, bootstrap_id, last_seq, pump_done_guard);
            Some(gate_tx)
        };

        Ok(AttachResourceGeneration {
            bootstrap_id,
            generation_last_seq,
            live_gate_tx,
            snapshot_gate,
            state_sync_bootstrap: outcome.and_then(|outcome| outcome.state_sync_bootstrap),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication_gate: Some(native_publication_gate_tx),
        })
    }

    /// Start the raw broadcast pump for one generation. An unrecoverable
    /// fault releases only this client's consumer state: the pane is shared.
    pub(super) fn spawn_output_pump(
        &self,
        channels: AttachResourcePumpChannels,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
        pump_done_guard: tokio_util::sync::DropGuard,
    ) {
        let ctx = crate::runtime::attach::OutputPumpContext {
            out_tx: self.out_tx.clone(),
            resize: self.terminal.resize.clone(),
            wire_terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            initial_bootstrap_id: bootstrap_id,
            client_id: self.client_id,
            client_caps: self.client_caps,
            profile: self.stream_profile,
            limits: self.bootstrap_limits,
            lag_label: "ATTACH_RESOURCE output pump",
            stale_skip: false,
            cancel: Some(channels.token.clone()),
            drain: Some(CancellationToken::new()),
            snapshot: Some(self.terminal.snapshot.clone()),
            last_seq: Some(generation_last_seq),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            terminal: self.terminal.clone(),
        };
        let state = self.state.clone();
        let connection_token = self.connection_token.clone();
        let client_id = self.client_id;
        pump::spawn_tracked(
            self.state,
            self.client_id,
            self.core,
            None,
            ctx.drain.clone(),
            async move {
                let _done_guard = pump_done_guard;
                let Some((start, output_rx)) = channels.published(ctx.profile).await else {
                    return;
                };
                if let Some(fault) =
                    crate::runtime::attach::run_started_output_pump(&ctx, start, output_rx).await
                {
                    crate::runtime::attach::release_after_pump_fault(
                        fault,
                        &state,
                        client_id,
                        &connection_token,
                    );
                }
            },
        );
    }

    /// Publish the atomic state-sync bootstrap the actor captured with
    /// registration, then open the live gate.
    pub(super) async fn finish_state_sync(
        &self,
        generation: &AttachResourceGeneration,
        state_sync: crate::terminal_actor::StateSyncBootstrap,
    ) -> Result<(), AttachResourceFailure> {
        self.publish_snapshot(generation, state_sync.snapshot, state_sync.base_seq)
            .await?;
        generation
            .generation_last_seq
            .store(state_sync.base_seq, std::sync::atomic::Ordering::Release);
        let _ = generation.live_gate_tx.send(true);
        Ok(())
    }

    /// Publish the negotiated bootstrap: a native checkpoint or a snapshot.
    pub(super) async fn finish_bootstrap(
        &self,
        generation: &mut AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if native_checkpoint_profile(self.stream_profile) {
            return self.finish_native(generation).await;
        }
        self.finish_snapshot(generation).await
    }

    /// Capture a native checkpoint bootstrap from the pane actor.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) async fn capture_native_bootstrap(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<crate::terminal_actor::NativeBootstrapReply, AttachResourceFailure> {
        use crate::runtime::attach::NativeRequestFailure;
        crate::runtime::attach::request_native_checkpoint(&self.terminal, |reply| {
            crate::terminal_actor::NativeBootstrapRequest::full_budget(
                self.client_id.0,
                self.terminal_id.clone(),
                self.stream_id,
                bootstrap_id,
                self.bootstrap_limits,
                reply,
            )
        })
        .await
        .map_err(|failure| match failure {
            NativeRequestFailure::Unsent => {
                AttachResourceFailure::internal("pane actor unavailable for native ATTACH_RESOURCE")
            }
            NativeRequestFailure::Dropped => {
                AttachResourceFailure::internal("pane actor dropped native ATTACH_RESOURCE")
            }
            NativeRequestFailure::Refused(error) => AttachResourceFailure {
                code: ErrorCode::CodecUnavailable,
                message: format!("native ATTACH_RESOURCE failed: {error}"),
            },
        })
    }

    /// Publish the native checkpoint bootstrap, cross the publication fence,
    /// and release both the live gate and the pump's first delta.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) async fn finish_native(
        &self,
        generation: &mut AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        let reply = self
            .capture_native_bootstrap(generation.bootstrap_id)
            .await?;
        let (cut, cursor) = crate::runtime::attach::publish_native_bootstrap(self.out_tx, reply)
            .await
            .map_err(|()| {
                AttachResourceFailure::internal("consumer went away during native ATTACH_RESOURCE")
            })?;
        let publication = crate::runtime::attach::activate_native_publication(
            &self.terminal,
            self.client_id.0,
            self.terminal_id.clone(),
            self.stream_id,
            generation.bootstrap_id,
            cursor,
        )
        .await
        .map_err(|_| {
            AttachResourceFailure::internal("pane actor unavailable at native publication fence")
        })?;
        let Some(publication_gate) = generation.native_publication_gate.take() else {
            return Err(AttachResourceFailure::internal(
                "native publication gate already consumed",
            ));
        };
        publication_gate.send(publication).map_err(|_| {
            AttachResourceFailure::internal(
                "native ATTACH_RESOURCE pump went away before publication",
            )
        })?;
        generation
            .generation_last_seq
            .store(cut, std::sync::atomic::Ordering::Release);
        let _ = generation.live_gate_tx.send(true);
        if let Some(gate) = generation.snapshot_gate.take() {
            let _ = gate.send(cut);
        }
        debug!(
            client_id = ?self.client_id,
            terminal_id = ?self.terminal_id,
            "native ATTACH_RESOURCE subscribed"
        );
        Ok(())
    }

    /// Both synthesized profiles replay bounded history before the current
    /// screen, using the same capability adaptation as session attachments.
    pub(super) async fn publish_snapshot(
        &self,
        generation: &AttachResourceGeneration,
        snapshot: crate::grid::SnapshotBytes,
        cut: u64,
    ) -> Result<(), AttachResourceFailure> {
        let (cols, rows) = (snapshot.cols, snapshot.rows);
        let adapted = crate::runtime::attach::adapt_bootstrap_snapshot(
            snapshot,
            self.client_caps,
            crate::runtime::attach::MAX_STAGED_BOOTSTRAP_BYTES,
        )
        .map_err(|()| {
            AttachResourceFailure::internal("resource snapshot exceeds bootstrap budget")
        })?;
        crate::runtime::attach::send_synthesized_bootstrap(
            self.out_tx,
            self.terminal_id.clone(),
            self.stream_id,
            generation.bootstrap_id,
            self.stream_profile,
            self.bootstrap_limits,
            cols,
            rows,
            cut,
            adapted.payloads,
        )
        .await
        .map_err(|()| {
            AttachResourceFailure::internal("consumer went away during resource bootstrap")
        })
    }

    /// Publish the authoritative snapshot, sent before the pump's first delta
    /// (the gate below releases it) and before the Ok reply.
    pub(super) async fn finish_snapshot(
        &self,
        generation: &mut AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        use crate::terminal_actor::SnapshotRequest;

        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        self.terminal
            .snapshot
            .send(SnapshotRequest {
                scrollback: Some(crate::resource::terminal::DEFAULT_REPLAY_SCROLLBACK_LINES),
                max_bytes: crate::runtime::attach::bootstrap_source_ceiling(
                    crate::runtime::attach::MAX_STAGED_BOOTSTRAP_BYTES,
                    self.client_caps,
                ),
                max_frames: crate::runtime::attach::MAX_STAGED_BOOTSTRAP_FRAMES,
                chunk_bytes: usize::try_from(self.bootstrap_limits.max_chunk_bytes()).map_err(
                    |_| AttachResourceFailure::internal("bootstrap chunk bound cannot fit host"),
                )?,
                reply: snapshot_tx,
            })
            .await
            .map_err(|_| {
                AttachResourceFailure::internal("pane actor unavailable for ATTACH_RESOURCE")
            })?;
        let Ok(Ok((snap, cut))) = snapshot_rx.await else {
            return Err(AttachResourceFailure::internal(
                "pane actor dropped the ATTACH_RESOURCE snapshot",
            ));
        };
        self.publish_snapshot(generation, snap, cut).await?;
        let _ = generation.live_gate_tx.send(true);
        generation
            .generation_last_seq
            .store(cut, std::sync::atomic::Ordering::Release);
        if let Some(gate) = generation.snapshot_gate.take() {
            let _ = gate.send(cut);
        }
        debug!(
            client_id = ?self.client_id,
            terminal_id = ?self.terminal_id,
            "ATTACH_RESOURCE subscribed"
        );
        Ok(())
    }
}
