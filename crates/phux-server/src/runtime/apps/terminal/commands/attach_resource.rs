//! `ATTACH_RESOURCE` session state and generation setup.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// Why an `ATTACH_RESOURCE` stage failed; [`AttachResourceSession::failed`]
/// rolls the partial generation back exactly once.
#[derive(Debug)]
pub(crate) struct AttachResourceFailure {
    /// Wire error code the caller receives.
    pub(crate) code: ErrorCode,
    /// Human-readable explanation attached to that code.
    pub(crate) message: String,
}

impl AttachResourceFailure {
    /// An internal fault with a fixed explanation — the dominant shape.
    pub(in crate::runtime) fn internal(message: &str) -> Self {
        Self {
            code: ErrorCode::InternalError,
            message: message.to_owned(),
        }
    }
}

/// The pump generation an attach established, plus the handshake artifacts
/// its bootstrap publication still has to consume.
pub(in crate::runtime) struct AttachResourceGeneration {
    /// Replica generation stamped on every frame this attach publishes.
    pub(in crate::runtime) bootstrap_id: phux_protocol::ids::BootstrapId,
    /// Shared cursor the pump keeps current so a replacement generation can
    /// resume from where this one stopped.
    pub(in crate::runtime) generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Releases the actor's live state-sync emission once the bootstrap is on
    /// the wire.
    pub(in crate::runtime) live_gate_tx: tokio::sync::watch::Sender<bool>,
    /// Releases the raw pump's first delta at the published cut. `None` when
    /// the actor's tick owns emission and no raw pump was spawned.
    pub(in crate::runtime) snapshot_gate: Option<oneshot::Sender<u64>>,
    /// Atomic synthesized bootstrap captured with state-sync registration.
    pub(in crate::runtime) state_sync_bootstrap: Option<phux_terminal::actor::StateSyncBootstrap>,
    /// Hands the pump its post-replay live receiver at the native publication
    /// fence.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::runtime) native_publication_gate:
        Option<oneshot::Sender<phux_terminal::actor::NativePublicationReply>>,
}

/// One in-flight `ATTACH_RESOURCE`: the resolved terminal plus the negotiated
/// connection context every stage needs. Every failure funnels through
/// [`Self::failed`].
pub(in crate::runtime) struct AttachResourceSession<'a> {
    pub(in crate::runtime) state: &'a SharedState,
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

    /// Undo a partial generation: cancel it, release any native lease, and
    /// detach the per-consumer state entry. The subscription is the caller's.
    pub(in crate::runtime) fn failed(
        &self,
        failure: AttachResourceFailure,
    ) -> AttachResourceFailure {
        use phux_terminal::actor::ConsumerDetachRequest;

        self.state.with_mut(|s| {
            s.cancel_attach_terminal_pump(self.client_id, self.core);
        });
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let _ = self
            .terminal
            .native_release
            .try_send(phux_terminal::actor::NativeReleaseRequest {
                owner: self.client_id.0,
            });
        let (reply, _ack) = oneshot::channel();
        let _ = self
            .terminal
            .consumer_detach
            .try_send(ConsumerDetachRequest {
                client_id: crate::runtime::wire_client(self.client_id),
                reply,
            });
        failure
    }

    /// Register the per-consumer state-sync entry (ADR-0018) so `FRAME_ACK`
    /// drives the actor's eviction loop. `None` when the terminal is not local
    /// or the actor refused.
    async fn register_consumer(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        live_gate: tokio::sync::watch::Receiver<bool>,
    ) -> Option<phux_terminal::actor::ConsumerAttachOutcome> {
        use phux_terminal::actor::ConsumerAttachRequest;

        let wire_terminal_id = self.terminal_id.local_id()?;
        let (reply, reply_rx) = oneshot::channel();
        self.terminal
            .consumer_attach
            .send(ConsumerAttachRequest {
                client_id: crate::runtime::wire_client(self.client_id),
                outbound: self.out_tx.clone(),
                wire_terminal_id,
                stream_id: self.stream_id,
                bootstrap_id,
                wants_state_sync: self.wants_state_sync(),
                image_protocols: self.client_caps.image_protocols,
                live_gate,
                state_sync_scrollback: Some(phux_terminal::actor::DEFAULT_REPLAY_SCROLLBACK_LINES),
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
    pub(in crate::runtime) async fn establish_generation(
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
            oneshot::channel::<phux_terminal::actor::NativePublicationReply>();
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
}

/// The one-shot gates one `ATTACH_RESOURCE` pump generation waits on before
/// it reaches steady state.
pub(in crate::runtime) struct AttachResourcePumpChannels {
    /// Cancels this generation when a replacement attach supersedes it.
    pub(in crate::runtime) token: CancellationToken,
    /// The pane's broadcast output, subscribed before the prior pump stopped.
    output_rx: tokio::sync::broadcast::Receiver<crate::resource::ResourceOutput>,
    /// Delivers the published cut, releasing the first forwarded delta.
    pub(in crate::runtime) snapshot_gate: oneshot::Receiver<u64>,
    /// Delivers the post-replay live receiver at the native publication fence.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::runtime) native_publication_gate:
        oneshot::Receiver<phux_terminal::actor::NativePublicationReply>,
}

impl AttachResourcePumpChannels {
    /// Wait for this generation's publication: the published cut, then (for
    /// a native profile) the post-replay live receiver. `None` when the
    /// generation was cancelled or abandoned first.
    pub(in crate::runtime) async fn published(
        self,
        #[cfg_attr(
            not(all(feature = "native-engine", not(target_arch = "wasm32"))),
            allow(unused_variables)
        )]
        profile: phux_protocol::caps::BootstrapStreamProfile,
    ) -> Option<(
        crate::runtime::attach::OutputPumpStart,
        tokio::sync::broadcast::Receiver<crate::resource::ResourceOutput>,
    )> {
        let published_cut = tokio::select! {
            () = self.token.cancelled() => return None,
            result = self.snapshot_gate => result.ok()?,
        };
        let mut start = crate::runtime::attach::OutputPumpStart {
            published_cut,
            replay: Vec::new(),
            live: None,
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if native_checkpoint_profile(profile) {
            let publication = self.native_publication_gate.await.ok()?;
            start.live = Some(publication.live);
            start.replay = publication.replay;
        }
        Some((start, self.output_rx))
    }
}
