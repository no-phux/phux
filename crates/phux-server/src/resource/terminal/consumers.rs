//! Per-consumer state-sync lifecycle for [`TerminalActor`]: consumer
//! registration and detach, `FRAME_ACK` handling, loss tolerance, and
//! the RTT-adaptive tick cadence.

use super::{
    ClientId, ConsumerAttachError, ConsumerAttachOutcome, ConsumerAttachRequest, ConsumerReference,
    ConsumerSyncState, DEFAULT_TICK_INTERVAL, GhosttyTerminal, LastAckedCursorMode, Outbound,
    RenderState, RttEstimator, StateSyncBootstrap, TICK_RESET_DEADBAND, TerminalActor, mpsc, trace,
    warn, watch,
};

impl TerminalActor {
    /// Register `client_id` as an attached consumer, priming its reference
    /// to the live grid (the ATTACH snapshot that follows brings the mirror
    /// to the same point, so the first delta is only what changed since).
    /// Re-registering overwrites the prior entry.
    #[allow(
        clippy::too_many_arguments,
        reason = "the arguments are the complete consumer generation identity and synchronization cut"
    )]
    pub(super) fn register_consumer_generation(
        &mut self,
        client_id: ClientId,
        outbound: mpsc::Sender<Outbound>,
        wire_terminal_id: u32,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        wants_state_sync: bool,
        live_gate: watch::Receiver<bool>,
        next_seq: u64,
    ) -> Result<(), ConsumerAttachError> {
        // Priming costs two full-grid renders; raw (pump-served) consumers
        // never read the reference, so only tick-managed ones pay.
        // `needs_initial_emit` would prime a raw one if it were ever ticked.
        let tick_managed = wants_state_sync || self.consumer_tick_emits;
        let (last_cursor_mode, reference) = if tick_managed {
            let canonical = self.terminal.borrow();
            let Some(terminal) = canonical.try_terminal() else {
                return Err(crate::grid::SynthesisError::TerminalUnavailable.into());
            };
            // A one-shot render state, so priming below can borrow the
            // shared synthesizer.
            let last_cursor_mode = {
                let mut render_state = RenderState::new()?;
                let snapshot = render_state.update(terminal)?;
                LastAckedCursorMode::capture(terminal, &snapshot)
            };
            // Prime so the first diff reports only changes from now.
            let mut reference = ConsumerReference::new();
            self.synth
                .borrow_mut()
                .prime_reference(terminal, &mut reference)?;
            (last_cursor_mode, reference)
        } else {
            (LastAckedCursorMode::unprimed(), ConsumerReference::new())
        };
        self.consumer_states.insert(
            client_id,
            ConsumerSyncState {
                reference,
                outbound,
                wire_terminal_id,
                stream_id,
                bootstrap_id,
                live_gate,
                // The bootstrap and this sequence share one cut.
                next_seq,
                last_acked_seq: 0,
                last_cursor_mode,
                // Walk once on the next tick even on a clean terminal.
                needs_initial_emit: true,
                behind: false,
                // Cold-start cadence until the first ack round-trip.
                rtt: RttEstimator::default(),
                emit_instants: std::collections::BTreeMap::new(),
                wants_state_sync,
                // Loss tolerance is opt-in via `enable_loss_tolerance`.
                loss_tolerant: false,
                acked_reference: ConsumerReference::new(),
                pending_refs: std::collections::BTreeMap::new(),
            },
        );
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn register_consumer(
        &mut self,
        client_id: ClientId,
        outbound: mpsc::Sender<Outbound>,
        wire_terminal_id: u32,
        wants_state_sync: bool,
    ) -> Result<(), ConsumerAttachError> {
        let (_live_gate_tx, live_gate) = watch::channel(true);
        self.register_consumer_generation(
            client_id,
            outbound,
            wire_terminal_id,
            phux_protocol::ids::StreamId::new(u64::from(client_id.get()) + 1)
                .expect("test stream id"),
            phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            wants_state_sync,
            live_gate,
            1,
        )
    }
    pub(super) fn handle_consumer_attach(&mut self, req: ConsumerAttachRequest) {
        fn byte_ceiling(
            max_bytes: usize,
            max_frames: usize,
            chunk_bytes: usize,
        ) -> Result<usize, ConsumerAttachError> {
            let chunk_frames = max_frames
                .checked_sub(2)
                .ok_or(crate::grid::SynthesisError::LimitExceeded)?;
            let frame_bytes = chunk_frames.saturating_mul(chunk_bytes);
            Ok(max_bytes.min(frame_bytes))
        }
        let ConsumerAttachRequest {
            client_id,
            outbound,
            wire_terminal_id,
            stream_id,
            bootstrap_id,
            wants_state_sync,
            state_sync_scrollback,
            bootstrap_max_bytes,
            bootstrap_max_frames,
            bootstrap_chunk_bytes,
            loss_tolerant,
            live_gate,
            reply,
        } = req;
        // Registration, snapshot, and priming are one actor turn.
        let tick_managed = self.consumer_tick_emits || wants_state_sync;
        let result = (|| {
            let base_seq = self.core.seq();
            let next_seq = if tick_managed {
                base_seq
                    .checked_add(1)
                    .ok_or(ConsumerAttachError::SequenceExhausted)?
            } else {
                1
            };
            let state_sync_bootstrap = if wants_state_sync {
                let max_bytes = byte_ceiling(
                    bootstrap_max_bytes,
                    bootstrap_max_frames,
                    bootstrap_chunk_bytes,
                )?;
                Some(StateSyncBootstrap {
                    snapshot: self
                        .synthesize_with_scrollback_bounded(state_sync_scrollback, max_bytes)?,
                    base_seq,
                })
            } else {
                None
            };
            self.register_consumer_generation(
                client_id,
                outbound,
                wire_terminal_id,
                stream_id,
                bootstrap_id,
                wants_state_sync,
                live_gate,
                next_seq,
            )?;
            if loss_tolerant && tick_managed {
                self.enable_loss_tolerance(client_id);
            }
            Ok(ConsumerAttachOutcome {
                tick_managed,
                state_sync_bootstrap,
            })
        })();
        if let Err(err) = &result {
            warn!(
                ?client_id,
                wire_terminal_id,
                error = %err,
                "consumer attach: atomic state-sync bootstrap failed",
            );
        } else {
            trace!(
                ?client_id,
                wire_terminal_id, tick_managed, "consumer attached at atomic state-sync cut"
            );
        }
        let _ = reply.send(result);
    }

    /// Switch a registered consumer to the advance-on-ack loss-tolerant
    /// model (ADR-0042), priming its acked reference to the live grid. No-op
    /// for an unknown consumer; a failed prime leaves it on emit-once.
    pub(super) fn enable_loss_tolerance(&mut self, client_id: ClientId) {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            // Needs the live grid; stay on emit-once while it is on loan.
            trace!(?client_id, "loss tolerance deferred: terminal is on loan");
            return;
        };
        let synth = &self.synth;
        let Some(state) = self.consumer_states.get_mut(&client_id) else {
            trace!(
                ?client_id,
                "enable_loss_tolerance for unregistered consumer; dropping"
            );
            return;
        };
        match synth
            .borrow_mut()
            .prime_reference(terminal, &mut state.acked_reference)
        {
            Ok(()) => {
                state.loss_tolerant = true;
                trace!(?client_id, "loss-tolerant state-sync enabled for consumer");
            }
            Err(err) => {
                warn!(
                    ?client_id,
                    error = %err,
                    "enable_loss_tolerance: priming acked reference failed; staying emit-once",
                );
            }
        }
    }

    /// Drop the per-consumer state for `client_id`, if present.
    pub(super) fn unregister_consumer(&mut self, client_id: ClientId) {
        let _ = self.consumer_states.remove(&client_id);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.release_native_owner(u64::from(client_id.get()));
    }

    /// Handle a cumulative `FRAME_ACK` for `seq` (SPEC §8.2).
    ///
    /// Under emit-once the reference already advanced on emit, so the ack
    /// tracks `last_acked_seq`, refreshes the cursor/mode capture, and feeds
    /// RTT. Stale or unknown acks are ignored. Returns `true` when a fresh
    /// RTT sample was folded in (the loop then re-evaluates the cadence).
    pub(super) fn on_generation_frame_ack(
        &mut self,
        client_id: ClientId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        seq: u64,
    ) -> bool {
        let Some(consumer) = self.consumer_states.get(&client_id) else {
            return false;
        };
        if consumer.stream_id != stream_id || consumer.bootstrap_id != bootstrap_id {
            trace!(
                ?client_id,
                ?stream_id,
                ?bootstrap_id,
                seq,
                "FRAME_ACK for stale stream generation; dropping",
            );
            return false;
        }
        self.on_frame_ack(client_id, seq)
    }

    pub(super) fn on_frame_ack(&mut self, client_id: ClientId, seq: u64) -> bool {
        // The test override that forces every consumer onto the tick.
        let force_all_consumers = self.consumer_tick_emits;
        let Some(consumer) = self.consumer_states.get_mut(&client_id) else {
            // An ack racing detach; steady-state, not a misuse.
            trace!(
                ?client_id,
                seq, "FRAME_ACK for unregistered consumer; dropping"
            );
            return false;
        };
        if !Self::accepts_frame_ack(consumer, client_id, seq, force_all_consumers) {
            return false;
        }
        consumer.last_acked_seq = seq;

        if consumer.loss_tolerant {
            Self::advance_acked_reference(consumer, seq);
        }
        let sampled = Self::fold_rtt_sample(client_id, consumer, seq);
        // Capture through the tick renderer: a second render state would
        // consume dirty bits without refreshing the tick's row cache.
        let canonical = self.terminal.borrow();
        if let Some(terminal) = canonical.try_terminal()
            && let Some(cm) = Self::capture_acked_cursor_mode(
                &mut self.synth.borrow_mut(),
                terminal,
                client_id,
                seq,
            )
        {
            consumer.last_cursor_mode = cm;
        }
        drop(canonical);

        trace!(
            ?client_id,
            seq, "FRAME_ACK applied: last_acked_seq advanced"
        );
        sampled
    }

    /// Read-only admission: a rejected ack changes nothing.
    fn accepts_frame_ack(
        consumer: &ConsumerSyncState,
        client_id: ClientId,
        seq: u64,
        force_all_consumers: bool,
    ) -> bool {
        // A raw consumer acks the pump's own seq space, unrelated to this
        // state; folding it in would corrupt accounting.
        if !force_all_consumers && !consumer.wants_state_sync {
            trace!(
                ?client_id,
                seq, "FRAME_ACK for raw-broadcast consumer; not a tick ack, dropping"
            );
            return false;
        }
        if seq <= consumer.last_acked_seq {
            // Cumulative: nothing new.
            trace!(
                ?client_id,
                seq,
                last_acked_seq = consumer.last_acked_seq,
                "FRAME_ACK older/duplicate; dropping",
            );
            return false;
        }
        if seq >= consumer.next_seq {
            // An ack beyond what was emitted is rejected.
            trace!(
                ?client_id,
                seq,
                next_seq = consumer.next_seq,
                "FRAME_ACK beyond emitted sequence; dropping"
            );
            return false;
        }
        true
    }

    /// Advance a loss-tolerant consumer's acked reference to the snapshot of
    /// the highest emitted `seq` this ack covers, dropping older snapshots
    /// (ADR-0042).
    fn advance_acked_reference(consumer: &mut ConsumerSyncState, seq: u64) {
        if let Some((&covered, _)) = consumer.pending_refs.range(..=seq).next_back()
            && let Some(snapshot) = consumer.pending_refs.remove(&covered)
        {
            consumer.acked_reference = snapshot;
        }
        // Drain only covered entries; preserve the in-flight tree in place.
        while consumer
            .pending_refs
            .first_key_value()
            .is_some_and(|(&key, _)| key <= seq)
        {
            consumer.pending_refs.pop_first();
        }
    }

    /// Turn the ack into an RTT sample from the newest covered emit instant,
    /// then prune every covered instant. `true` when a sample was folded.
    fn fold_rtt_sample(client_id: ClientId, consumer: &mut ConsumerSyncState, seq: u64) -> bool {
        let now = tokio::time::Instant::now();
        let rtt_sample = consumer
            .emit_instants
            .range(..=seq)
            .next_back()
            .map(|(_, &emitted_at)| now.saturating_duration_since(emitted_at));
        while consumer
            .emit_instants
            .first_key_value()
            .is_some_and(|(&key, _)| key <= seq)
        {
            consumer.emit_instants.pop_first();
        }
        let Some(sample) = rtt_sample else {
            return false;
        };
        crate::perf::CONSUMER_ACK_RTT.record_duration(sample);
        consumer.rtt.observe(sample);
        trace!(
            ?client_id,
            seq,
            rtt_ms = sample.as_secs_f64() * 1000.0,
            srtt_ms = consumer.rtt.smoothed().map(|d| d.as_secs_f64() * 1000.0),
            "FRAME_ACK: RTT sample folded into EMA",
        );
        true
    }

    /// Capture the cursor/mode state to pair with an ack; `None` (keep the
    /// prior capture) if the render state fails.
    fn capture_acked_cursor_mode(
        synth: &mut crate::grid::SnapshotSynthesizer<'static>,
        terminal: &GhosttyTerminal<'static, '_>,
        client_id: ClientId,
        seq: u64,
    ) -> Option<LastAckedCursorMode> {
        match synth.metadata_snapshot(terminal) {
            Ok(snapshot) => Some(LastAckedCursorMode::capture(terminal, &snapshot)),
            Err(err) => {
                warn!(
                    ?client_id,
                    seq,
                    error = %err,
                    "FRAME_ACK: cursor/mode capture update failed; keeping prior capture",
                );
                None
            }
        }
    }

    /// The shared tick interval: the minimum desired interval over all
    /// consumers, so the fastest peer sets the cadence (slower peers just see
    /// more empty diffs). [`DEFAULT_TICK_INTERVAL`] with no samples.
    pub(super) fn adaptive_tick_interval(&self) -> std::time::Duration {
        self.consumer_states
            .values()
            .map(|s| s.rtt.desired_tick_interval())
            .min()
            .unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    /// Rebuild the shared timer at `desired` when it differs from `current`
    /// by more than [`TICK_RESET_DEADBAND`]. `Interval`'s period is fixed, so
    /// a new cadence means a new interval, first tick one period out.
    pub(super) fn rearm_tick(
        tick: &mut tokio::time::Interval,
        current: &mut std::time::Duration,
        desired: std::time::Duration,
    ) {
        if current.abs_diff(desired) < TICK_RESET_DEADBAND {
            return;
        }
        *current = desired;
        let mut next = tokio::time::interval_at(tokio::time::Instant::now() + desired, desired);
        next.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        *tick = next;
    }

    /// Test-only: number of consumers currently registered.
    #[cfg(test)]
    pub fn consumer_count(&self) -> usize {
        self.consumer_states.len()
    }

    /// Test-only: borrow the per-consumer state for `client_id`.
    #[cfg(test)]
    pub fn consumer_state(&self, client_id: ClientId) -> Option<&ConsumerSyncState> {
        self.consumer_states.get(&client_id)
    }
}
