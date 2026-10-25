//! `ATTACH_RESOURCE` output pump and bootstrap completion.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

impl AttachResourceSession<'_> {
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
    async fn capture_native_bootstrap(
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
    async fn finish_native(
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
    async fn publish_snapshot(
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
    async fn finish_snapshot(
        &self,
        generation: &mut AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        use crate::terminal_actor::SnapshotRequest;

        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        self.terminal
            .snapshot
            .send(SnapshotRequest {
                scrollback: Some(crate::resource::terminal::DEFAULT_REPLAY_SCROLLBACK_LINES),
                image_protocols: self.client_caps.image_protocols,
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
