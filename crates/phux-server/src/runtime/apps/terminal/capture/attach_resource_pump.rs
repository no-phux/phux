//! `ATTACH_RESOURCE` output pump and bootstrap completion.

use phux_protocol::wire::frame::ErrorCode;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::runtime::apps::terminal::capture::attach_resource::{
    AttachResourceGeneration, AttachResourcePumpChannels, AttachResourceSession, GenerationDelivery,
};
use crate::runtime::commands::AttachResourceFailure;
use crate::runtime::commands::attach_terminal::native_checkpoint_profile;

impl AttachResourceSession<'_> {
    /// Start the raw broadcast pump for one generation. An unrecoverable
    /// fault releases only this client's consumer state: the pane is shared.
    pub(in crate::runtime) fn spawn_output_pump(
        &self,
        channels: AttachResourcePumpChannels,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
        pump_done_guard: tokio_util::sync::DropGuard,
    ) {
        let ctx = crate::runtime::apps::terminal::attach::OutputPumpContext {
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
            terminal: self.terminal.clone(),
        };
        let host = self.host.clone();
        let connection_token = self.connection_token.clone();
        let lifetime = channels.token.clone().drop_guard();
        self.host.spawn(
            self.core,
            None,
            ctx.drain.clone(),
            async move {
                let _done_guard = pump_done_guard;
                let _lifetime = lifetime;
                let Some((start, output_rx)) = channels.published().await else {
                    return;
                };
                if let Some(fault) =
                    crate::runtime::apps::terminal::attach::run_started_output_pump(
                        &ctx, start, output_rx,
                    )
                    .await
                {
                    host.retire_terminal_fault(fault, &connection_token);
                }
            },
        );
    }

    /// Capture, queue the complete bootstrap and transfer the single lifetime
    /// in one continuation. Cancellation while any mailbox is full drops both
    /// the actor registration and the exact tracked generation.
    pub(in crate::runtime) async fn publish_generation(
        &self,
        generation: AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        let lifetime = generation.lifetime.clone();
        tokio::select! {
            biased;
            () = self.connection_token.cancelled() => Err(AttachResourceFailure::internal("connection cancelled during publication")),
            () = lifetime.cancelled() => Err(AttachResourceFailure::internal("generation superseded during publication")),
            result = self.complete_publication(generation) => result,
        }
    }

    async fn complete_publication(&self, generation: AttachResourceGeneration) -> Result<(), AttachResourceFailure> {
        let AttachResourceGeneration { bootstrap_id, generation_last_seq, delivery, registration, pending_pump, .. } = generation;
        match delivery {
            GenerationDelivery::Tick { bootstrap, live } => {
                self.publish_snapshot(bootstrap_id, bootstrap.snapshot, bootstrap.base_seq).await?;
                generation_last_seq.store(bootstrap.base_seq, std::sync::atomic::Ordering::Release);
                live.send(true).map_err(|_| AttachResourceFailure::internal("tick consumer retired before publication"))?;
            }
            GenerationDelivery::Raw { gate } => {
                let start = if native_checkpoint_profile(self.stream_profile) {
                    self.finish_native(bootstrap_id).await?
                } else {
                    self.finish_snapshot(bootstrap_id).await?
                };
                generation_last_seq.store(start.published_cut, std::sync::atomic::Ordering::Release);
                gate.send(start).map_err(|_| AttachResourceFailure::internal("output pump retired before publication"))?;
            }
        }
        // No await between opening the gate and handing off lifetime ownership.
        registration.published();
        let _ = pending_pump.disarm();
        Ok(())
    }

    /// Capture a native checkpoint bootstrap from the pane actor.
    async fn capture_native_bootstrap(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<phux_terminal_api::actor::NativeBootstrapReply, AttachResourceFailure> {
        use crate::runtime::apps::terminal::attach::NativeRequestFailure;
        crate::runtime::apps::terminal::attach::request_native_checkpoint(&self.terminal, |reply| {
            phux_terminal_api::actor::NativeBootstrapRequest::full_budget(
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

    /// Cross the native fence before handing the cut and receiver to the pump.
    async fn finish_native(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<crate::runtime::apps::terminal::attach::OutputPumpStart, AttachResourceFailure> {
        let reply = self.capture_native_bootstrap(bootstrap_id).await?;
        let (cut, capture) = crate::runtime::apps::terminal::attach::publish_native_bootstrap(self.out_tx, reply)
            .await.map_err(|()| AttachResourceFailure::internal("consumer went away during native bootstrap"))?;
        let publication = crate::runtime::apps::terminal::attach::activate_native_publication(
            &self.terminal, capture,
        ).await.map_err(|_| AttachResourceFailure::internal("pane actor unavailable at native publication fence"))?;
        let mut start = crate::runtime::apps::terminal::attach::OutputPumpStart {
            superseded: false, published_cut: cut, replay: Vec::new(), live: None, interest: None,
        };
        start.adopt_native_publication(publication);
        Ok(start)
    }

    /// Both synthesized profiles replay bounded history before the current
    /// screen, using the same capability adaptation as session attachments.
    async fn publish_snapshot(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        snapshot: phux_terminal_api::grid::SnapshotBytes,
        cut: u64,
    ) -> Result<(), AttachResourceFailure> {
        let (cols, rows) = (snapshot.cols, snapshot.rows);
        let adapted = crate::runtime::apps::terminal::attach::adapt_bootstrap_snapshot(
            &self.terminal.presentation,
            snapshot,
            self.client_caps,
            crate::runtime::apps::terminal::attach::MAX_STAGED_BOOTSTRAP_BYTES,
        )
        .map_err(|()| {
            AttachResourceFailure::internal("resource snapshot exceeds bootstrap budget")
        })?;
        crate::runtime::apps::terminal::attach::send_synthesized_bootstrap(
            self.out_tx,
            self.terminal_id.clone(),
            self.stream_id,
            bootstrap_id,
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
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<crate::runtime::apps::terminal::attach::OutputPumpStart, AttachResourceFailure> {
        use phux_terminal_api::actor::SnapshotRequest;

        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        self.terminal
            .snapshot
            .send(SnapshotRequest {
                scrollback: Some(phux_terminal_api::actor::DEFAULT_REPLAY_SCROLLBACK_LINES),
                image_protocols: self.client_caps.image_protocols,
                max_bytes: crate::runtime::apps::terminal::attach::bootstrap_source_ceiling(
                    &self.terminal.presentation,
                    crate::runtime::apps::terminal::attach::MAX_STAGED_BOOTSTRAP_BYTES,
                    self.client_caps,
                ),
                max_frames: crate::runtime::apps::terminal::attach::MAX_STAGED_BOOTSTRAP_FRAMES,
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
        self.publish_snapshot(bootstrap_id, snap, cut).await?;
        debug!(client_id = ?self.client_id, terminal_id = ?self.terminal_id, "ATTACH_RESOURCE subscribed");
        Ok(crate::runtime::apps::terminal::attach::OutputPumpStart {
            superseded: false, published_cut: cut, replay: Vec::new(), live: None,
        interest: None,
        })
    }
}
