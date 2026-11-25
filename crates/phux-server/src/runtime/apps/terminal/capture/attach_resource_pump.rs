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
        lease: &crate::runtime::apps::StreamLease,
    ) -> bool {
        let bootstrap_id = lease.bootstrap;
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
            last_seq: Some(std::sync::Arc::clone(&lease.last_seq)),
            terminal: self.terminal.clone(),
        };
        let owner = lease.clone();
        lease.spawn(
            None,
            ctx.drain.clone(),
            async move {
                let Some((start, output_rx)) = channels.published().await else {
                    return;
                };
                if let Some(fault) =
                    crate::runtime::apps::terminal::attach::run_started_output_pump(
                        &ctx, start, output_rx,
                    )
                    .await
                {
                    owner.retire_terminal_fault(fault);
                }
            },
        )
    }

    /// Capture, queue the complete bootstrap and transfer the single lifetime
    /// in one continuation. Cancellation while any mailbox is full drops both
    /// the actor registration and the exact tracked generation.
    pub(in crate::runtime) async fn publish_generation(
        &self,
        generation: AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        let lease = generation.reservation.lease.clone();
        lease.run(self.complete_publication(generation)).await
            .unwrap_or_else(|| Err(AttachResourceFailure::internal("generation superseded during publication")))
    }

    async fn complete_publication(&self, generation: AttachResourceGeneration) -> Result<(), AttachResourceFailure> {
        let AttachResourceGeneration { reservation, delivery, registration } = generation;
        let lease = reservation.lease.clone();
        match delivery {
            GenerationDelivery::Tick { bootstrap, live } => {
                self.publish_snapshot(&lease, bootstrap.snapshot, bootstrap.base_seq).await?;
                if !reservation.published(|| {
                    lease.last_seq.store(bootstrap.base_seq, std::sync::atomic::Ordering::Release);
                    live.send(true).is_ok()
                }) { return Err(AttachResourceFailure::internal("tick consumer retired before publication")); }
            }
            GenerationDelivery::Raw { gate } => {
                let start = if native_checkpoint_profile(self.stream_profile) {
                    self.finish_native(&lease).await?
                } else {
                    self.finish_snapshot(&lease).await?
                };
                if !reservation.published(|| {
                    lease.last_seq.store(start.published_cut, std::sync::atomic::Ordering::Release);
                    gate.send(start).is_ok()
                }) { return Err(AttachResourceFailure::internal("output pump retired before publication")); }
            }
        }
        // No await between opening the gate and handing off lifetime ownership.
        registration.published();
        Ok(())
    }

    /// Capture a native checkpoint bootstrap from the pane actor.
    async fn capture_native_bootstrap(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<phux_terminal_api::actor::NativeBootstrapReply, AttachResourceFailure> {
        use crate::runtime::apps::terminal::attach::{NativeRequestFailure, request_native_checkpoint};
        use crate::runtime::spawn::terminal::publication::past_invalidation;
        past_invalidation(|| {
            request_native_checkpoint(&self.terminal, |reply| {
                phux_terminal_api::actor::NativeBootstrapRequest::full_budget(
                    self.client_id.0,
                    self.terminal_id.clone(),
                    self.stream_id,
                    bootstrap_id,
                    self.bootstrap_limits,
                    reply,
                )
            })
        })
        .await
        .map_err(|failure| match failure {
            NativeRequestFailure::Unsent => {
                AttachResourceFailure::internal("pane actor unavailable for native ATTACH_RESOURCE")
            }
            NativeRequestFailure::Dropped => {
                AttachResourceFailure::internal("pane actor dropped native ATTACH_RESOURCE")
            }
            NativeRequestFailure::Refused(error) => AttachResourceFailure { generation: None,
                code: ErrorCode::CodecUnavailable,
                message: format!("native ATTACH_RESOURCE failed: {error}"),
            },
        })
    }

    /// Cross the native fence before handing the cut and receiver to the pump.
    async fn finish_native(
        &self,
        lease: &crate::runtime::apps::StreamLease,
    ) -> Result<crate::runtime::apps::terminal::attach::OutputPumpStart, AttachResourceFailure> {
        let reply = self.capture_native_bootstrap(lease.bootstrap).await?;
        let cut = reply.base_seq;
        if !lease.queue_frames(self.out_tx, reply.frames).await { return Err(AttachResourceFailure::internal("consumer went away during native bootstrap")); }
        let capture = reply.capture;
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
        lease: &crate::runtime::apps::StreamLease,
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
        let frames = crate::runtime::apps::terminal::attach::synthesized_bootstrap_frames(
            self.terminal_id.clone(),
            self.stream_id,
            lease.bootstrap,
            self.stream_profile,
            self.bootstrap_limits,
            cols,
            rows,
            cut,
            adapted.payloads,
        ).map_err(|()| AttachResourceFailure::internal("resource bootstrap exceeds negotiated limits"))?;
        if !lease.queue_frames(self.out_tx, frames).await { return Err(AttachResourceFailure::internal("consumer went away during resource bootstrap")); }
        Ok(())
    }

    /// Publish the authoritative snapshot, sent before the pump's first delta
    /// (the gate below releases it) and before the Ok reply.
    async fn finish_snapshot(
        &self,
        lease: &crate::runtime::apps::StreamLease,
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
        self.publish_snapshot(lease, snap, cut).await?;
        debug!(client_id = ?self.client_id, terminal_id = ?self.terminal_id, "ATTACH_RESOURCE subscribed");
        Ok(crate::runtime::apps::terminal::attach::OutputPumpStart {
            superseded: false, published_cut: cut, replay: Vec::new(), live: None,
        interest: None,
        })
    }
}

#[cfg(test)]
#[path = "attach_capture_tests.rs"]
mod tests;
