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
    pub(super) reservation: crate::runtime::apps::StreamReservation,
    pub(super) delivery: GenerationDelivery,
    pub(super) registration: phux_terminal_api::actor::ConsumerRegistrationGuard,
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
    pub(in crate::runtime) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(in crate::runtime) terminal_id: &'a phux_protocol::ids::ResourceId,
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
        send.await.ok()?;
        let outcome = reply_rx.await.ok()?.ok()?;
        Some(outcome)
    }

    /// Allocate the pump generation for this attach: register the consumer,
    /// tombstone whatever generation it replaces, and start the raw output
    /// pump unless the actor's tick owns emission.
    pub(in crate::runtime) async fn establish_generation(
        &self,
        reservation: crate::runtime::apps::StreamReservation,
    ) -> Result<AttachResourceGeneration, AttachResourceFailure> {
        let lease = reservation.lease.clone();
        lease.run(self.establish_reserved(reservation)).await
            .unwrap_or_else(|| Err(AttachResourceFailure::internal("ATTACH_RESOURCE reservation superseded")))
    }

    async fn establish_reserved(&self, reservation: crate::runtime::apps::StreamReservation) -> Result<AttachResourceGeneration, AttachResourceFailure> {
        let lease = reservation.lease.clone();
        let bootstrap_id = lease.bootstrap;
        // Subscribe before stopping the old pump so replacement never loses a
        // byte emitted in the handoff window. The new receiver remains gated
        // until this generation reaches READY.
        let output_rx = self.handle.output.subscribe();
        if !reservation.ready().await {
            return Err(AttachResourceFailure::internal("ATTACH_RESOURCE superseded while retiring predecessor"));
        }
        let token = lease.cancellation.clone();
        let (live_gate_tx, live_gate_rx) = tokio::sync::watch::channel(false);

        let registration = phux_terminal_api::actor::ConsumerRegistrationGuard::new(&token);
        let outcome = self
            .register_consumer(bootstrap_id, live_gate_rx, &registration)
            .await;
        if token.is_cancelled() {
            return Err(AttachResourceFailure::internal("ATTACH_RESOURCE cancelled"));
        }
        if self.wants_state_sync() && outcome.is_none() {
            return Err(AttachResourceFailure::internal(
                "ATTACH_RESOURCE state-sync registration failed",
            ));
        }
        let tick_managed = outcome.as_ref().is_some_and(|outcome| outcome.tick_managed);
        if let Some((prior_bootstrap_id, prior_last_seq)) = &reservation.prior
            && !lease.queue(self.out_tx, FrameKind::BootstrapTombstone {
                    terminal_id: self.terminal_id.clone(),
                    stream_id: self.stream_id,
                    bootstrap_id: *prior_bootstrap_id,
                    reason: phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
                    last_valid_seq: prior_last_seq.load(std::sync::atomic::Ordering::Acquire),
                }).await
        {
            return Err(AttachResourceFailure::internal(
                "consumer went away during ATTACH_RESOURCE replacement",
            ));
        }

        let delivery = if tick_managed {
            let Some(bootstrap) = outcome.and_then(|outcome| outcome.state_sync_bootstrap) else {
                return Err(AttachResourceFailure::internal("tick consumer omitted its atomic bootstrap"));
            };
            GenerationDelivery::Tick { bootstrap, live: live_gate_tx }
        } else {
            let (gate, published) = oneshot::channel();
            let channels = AttachResourcePumpChannels { token, output_rx, published };
            if !self.spawn_output_pump(channels, &lease) {
                return Err(AttachResourceFailure::internal("ATTACH_RESOURCE superseded before pump handoff"));
            }
            GenerationDelivery::Raw { gate }
        };
        Ok(AttachResourceGeneration {
            reservation,
            delivery,
            registration,
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
