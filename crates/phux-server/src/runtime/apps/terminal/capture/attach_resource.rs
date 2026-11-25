//! `ATTACH_RESOURCE` session state and generation setup.

use phux_protocol::caps::{BootstrapLimits, ClientCapabilities};
use phux_protocol::wire::frame::FrameKind;
use phux_terminal_api::actor::TerminalHandle;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::resource::ResourceHandle;
use crate::runtime::commands::AttachResourceFailure;
use crate::state::{ClientId, Outbound};

/// One unpublished cut and its lifetime. Only complete publication can hand
/// its interest to the tracked host generation.
pub(in crate::runtime) struct AttachResourceGeneration {
    pub(super) bootstrap_id: phux_protocol::ids::BootstrapId,
    pub(super) generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub(super) lifetime: CancellationToken,
    pub(super) delivery: GenerationDelivery,
    pub(super) registration: phux_terminal_api::actor::ConsumerRegistrationGuard,
    pub(super) pending_pump: tokio_util::sync::DropGuard,
}

pub(super) enum GenerationDelivery {
    Tick {
        bootstrap: phux_terminal_api::actor::StateSyncBootstrap,
        live: tokio::sync::watch::Sender<bool>,
    },
    Raw {
        gate: oneshot::Sender<crate::runtime::apps::terminal::attach::OutputPumpStart>,
    },
}

/// One in-flight `ATTACH_RESOURCE`: the resolved terminal plus the negotiated
/// connection context every stage needs.
pub(in crate::runtime) struct AttachResourceSession<'a> {
    pub(in crate::runtime) host: &'a crate::runtime::apps::StreamHost,
    pub(in crate::runtime) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(in crate::runtime) connection_token: &'a CancellationToken,
    pub(in crate::runtime) terminal_id: &'a phux_protocol::ids::ResourceId,
    pub(in crate::runtime) core: phux_core::ids::ResourceId,
    /// The resource's generic channels (output, consumers, control).
    pub(in crate::runtime) handle: ResourceHandle,
    /// The Terminal facet of `handle`, resolved once at entry.
    pub(in crate::runtime) terminal: TerminalHandle,
    pub(in crate::runtime) client_id: ClientId,
    pub(in crate::runtime) stream_id: phux_protocol::ids::StreamId,
    pub(in crate::runtime) client_caps: ClientCapabilities,
    pub(in crate::runtime) stream_profile: phux_protocol::caps::BootstrapStreamProfile,
    pub(in crate::runtime) bootstrap_limits: BootstrapLimits,
}

impl AttachResourceSession<'_> {
    /// Whether this connection negotiated the actor-emitted state-sync stream
    /// at HELLO.
    const fn wants_state_sync(&self) -> bool {
        matches!(
            self.client_caps.output_mode,
            phux_protocol::caps::OutputMode::StateSync
        )
    }

    /// Register the per-consumer state-sync entry (ADR-0018) so `FRAME_ACK`
    /// drives the actor's eviction loop. `None` when the terminal is not local
    /// or the actor refused.
    async fn register_consumer(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        live_gate: tokio::sync::watch::Receiver<bool>,
        lifetime: &CancellationToken,
        registration: &phux_terminal_api::actor::ConsumerRegistrationGuard,
    ) -> Option<phux_terminal_api::actor::ConsumerAttachOutcome> {
        use phux_terminal_api::actor::ConsumerAttachRequest;

        let wire_terminal_id = self.terminal_id.local_id()?;
        let (reply, reply_rx) = oneshot::channel();
        let send = self.terminal.consumer_attach.send(ConsumerAttachRequest {
            client_id: crate::runtime::wire_client(self.client_id),
            outbound: self.out_tx.clone(),
            wire_terminal_id,
            stream_id: self.stream_id,
            bootstrap_id,
            cancellation: registration.cancellation(),
            wants_state_sync: self.wants_state_sync(),
            image_protocols: self.client_caps.image_protocols,
            live_gate,
            state_sync_scrollback: Some(phux_terminal_api::actor::DEFAULT_REPLAY_SCROLLBACK_LINES),
            bootstrap_max_bytes: crate::runtime::apps::terminal::attach::bootstrap_source_ceiling(
                &self.terminal.presentation,
                crate::runtime::apps::terminal::attach::MAX_STAGED_BOOTSTRAP_BYTES,
                self.client_caps,
            ),
            bootstrap_max_frames:
                crate::runtime::apps::terminal::attach::MAX_STAGED_BOOTSTRAP_FRAMES,
            bootstrap_chunk_bytes: usize::try_from(self.bootstrap_limits.max_chunk_bytes()).ok()?,
            // Reliable transport: emit-once is correct (ADR-0042).
            loss_tolerant: false,
            reply,
        });
        tokio::select! {
            biased;
            () = self.connection_token.cancelled() => return None,
            () = lifetime.cancelled() => return None,
            result = send => result.ok()?,
        }
        let outcome = tokio::select! {
            biased;
            () = self.connection_token.cancelled() => return None,
            () = lifetime.cancelled() => return None,
            result = reply_rx => result.ok()?.ok()?,
        };
        Some(outcome)
    }

    /// Allocate the pump generation for this attach: register the consumer,
    /// tombstone whatever generation it replaces, and start the raw output
    /// pump unless the actor's tick owns emission.
    pub(in crate::runtime) async fn establish_generation(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<AttachResourceGeneration, AttachResourceFailure> {
        // Subscribe before stopping the old pump so replacement never loses a
        // byte emitted in the handoff window. The new receiver remains gated
        // until this generation reaches READY.
        let output_rx = self.handle.output.subscribe();
        self.host.stop(self.core).await;
        let (token, pump_done, generation_last_seq, prior) = self.host.replace(self.core, bootstrap_id);
        let pending_pump = token.clone().drop_guard();
        let mut pump_done_guard = Some(pump_done.drop_guard());
        if let Some((_, prior_done, _)) = &prior {
            prior_done.cancelled().await;
        }
        let (live_gate_tx, live_gate_rx) = tokio::sync::watch::channel(false);

        let registration = phux_terminal_api::actor::ConsumerRegistrationGuard::new(&token);
        let outcome = self
            .register_consumer(bootstrap_id, live_gate_rx, &token, &registration)
            .await;
        if self.connection_token.is_cancelled() || token.is_cancelled() {
            return Err(AttachResourceFailure::internal("ATTACH_RESOURCE cancelled"));
        }
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

        let lifetime = token.clone();
        let delivery = if tick_managed {
            let Some(bootstrap) = outcome.and_then(|outcome| outcome.state_sync_bootstrap) else {
                return Err(AttachResourceFailure::internal("tick consumer omitted its atomic bootstrap"));
            };
            GenerationDelivery::Tick { bootstrap, live: live_gate_tx }
        } else {
            let Some(pump_done_guard) = pump_done_guard.take() else {
                return Err(AttachResourceFailure::internal("pump generation lost its completion guard"));
            };
            let (gate, published) = oneshot::channel();
            let channels = AttachResourcePumpChannels { token, output_rx, published };
            self.spawn_output_pump(channels, bootstrap_id, std::sync::Arc::clone(&generation_last_seq), pump_done_guard);
            GenerationDelivery::Raw { gate }
        };
        Ok(AttachResourceGeneration {
            bootstrap_id,
            generation_last_seq,
            lifetime,
            delivery,
            registration,
            pending_pump,
        })
    }
}

/// The one-shot gates one `ATTACH_RESOURCE` pump generation waits on before
/// it reaches steady state.
pub(in crate::runtime) struct AttachResourcePumpChannels {
    /// Cancels this generation when a replacement attach supersedes it.
    pub(in crate::runtime) token: CancellationToken,
    /// The pane's broadcast output, subscribed before the prior pump stopped.
    output_rx: tokio::sync::broadcast::Receiver<crate::resource::ResourceOutput>,
    /// One atomic raw/native handoff: the cut, replay and post-cut live receiver.
    published: oneshot::Receiver<crate::runtime::apps::terminal::attach::OutputPumpStart>,
}

impl AttachResourcePumpChannels {
    /// Wait for the single complete cut/replay/live handoff. `None` when the
    /// generation was cancelled or abandoned first.
    pub(in crate::runtime) async fn published(self) -> Option<(
        crate::runtime::apps::terminal::attach::OutputPumpStart,
        tokio::sync::broadcast::Receiver<crate::resource::ResourceOutput>,
    )> {
        let start = tokio::select! {
            biased;
            () = self.token.cancelled() => return None,
            result = self.published => result.ok()?,
        };
        Some((start, self.output_rx))
    }
}

#[cfg(test)]
#[path = "attach_resource_tests.rs"]
mod tests;
