//! Per-consumer state-sync lifecycle for [`TerminalActor`]: consumer
//! registration and detach, `FRAME_ACK` handling, loss tolerance, and
//! the RTT-adaptive tick cadence.

use super::{
    ClientId, ConsumerAttachError, ConsumerAttachOutcome, ConsumerAttachRequest, ConsumerReference,
    ConsumerSyncState, DEFAULT_TICK_INTERVAL, LastAckedCursorMode, Outbound, RenderState,
    RttEstimator, StateSyncBootstrap, TICK_RESET_DEADBAND, TerminalActor, mpsc, trace, warn, watch,
};

impl TerminalActor {
    /// Register `client_id` as an attached consumer (phux-q0e.2).
    ///
    /// Allocates a fresh `RenderState`, primes it against the live
    /// terminal (`update` + manual `set_dirty(Clean)` walk), and stores
    /// the resulting [`ConsumerSyncState`] in `consumer_states`.
    ///
    /// Why prime + clear: the runtime's ATTACH path emits a
    /// `TERMINAL_SNAPSHOT` immediately after this call returns, which
    /// brings the consumer's mirror Terminal up to the current
    /// canonical state. The per-consumer reference must reflect that same
    /// reference point — otherwise the first incremental emission would
    /// treat every row as changed and re-paint the screen the snapshot
    /// just installed.
    ///
    /// Idempotent: re-attaching the same `client_id` (e.g. on a runtime
    /// bug) overwrites the prior entry.
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
        // Priming the per-consumer reference + cursor/mode capture costs two
        // full-grid render passes, but a raw broadcast-pump consumer (the
        // human attach path) never reads either: the tick serves only
        // tick-managed consumers and `FRAME_ACK` is dropped for raw ones, and
        // `wants_state_sync` is fixed at registration with no flip path. So
        // do the work only when this consumer is actually tick-managed; a raw
        // consumer attaches with an empty reference and a placeholder capture.
        // (If it were ever tick-served, `needs_initial_emit` forces a full
        // pass that primes both — see the tick emit gate.)
        let tick_managed = wants_state_sync || self.consumer_tick_emits;
        let (last_cursor_mode, reference) = if tick_managed {
            let terminal = self.terminal.borrow();
            // Cursor + DEC mode capture happens against a one-shot
            // `RenderState` so we don't conflict with the shared
            // synthesizer's borrow used to prime the reference below.
            let last_cursor_mode = {
                let mut render_state = RenderState::new()?;
                let snapshot = render_state.update(&terminal)?;
                LastAckedCursorMode::capture(&terminal, &snapshot)
            };
            // Prime the reference against the live terminal so the next
            // `synthesize_against_reference` emits only deltas from *now* —
            // the `TERMINAL_SNAPSHOT` the runtime emits right after this call
            // already brings the consumer's mirror to this same point.
            let mut reference = ConsumerReference::new();
            self.synth
                .borrow_mut()
                .prime_reference(&terminal, &mut reference)?;
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
                // The synthesized bootstrap and this sequence share one actor
                // cut, so the first live delta is exactly `base_seq + 1`.
                next_seq,
                last_acked_seq: 0,
                last_cursor_mode,
                // Force one synthesis pass on the next tick even if the
                // terminal is Clean since the previous tick (phux-4l0).
                needs_initial_emit: true,
                // Fresh consumer is not behind; `needs_initial_emit` already
                // guarantees its first pass runs.
                behind: false,
                // No RTT sample yet — runs at the cold-start default until the
                // first FRAME_ACK round-trip lands (phux-q0e.5).
                rtt: RttEstimator::default(),
                emit_instants: std::collections::BTreeMap::new(),
                wants_state_sync,
                // Loss-tolerance is opt-in and off by default (phux-v45.8):
                // the reliable-transport emit-once model is the norm. The
                // runtime flips it on via `enable_loss_tolerance` for a
                // forwarded/lossy leg right after registration.
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
        // Registration, synthesized snapshot, and reference priming are one
        // actor turn. No PTY event can land between the snapshot cut and the
        // reference it installs.
        let tick_managed = self.consumer_tick_emits || wants_state_sync;
        let result = (|| {
            let base_seq = self.raw_seq;
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

    /// Switch an already-registered consumer to the advance-on-ack
    /// loss-tolerant emission model (phux-v45.8, ADR-0042).
    ///
    /// Idempotent-ish: sets [`ConsumerSyncState::loss_tolerant`] and primes
    /// [`ConsumerSyncState::acked_reference`] to the live grid so the first
    /// post-enable tick emits only deltas from *now* (the consumer's
    /// `TERMINAL_SNAPSHOT` brought its mirror to this same point). Silent no-op
    /// if the consumer is not registered (raced against detach). A failure to
    /// prime the reference leaves the consumer on the emit-once path (the
    /// reference stays empty, so the first tick would repaint everything — safe,
    /// just not yet loss-tolerant); logged, not fatal.
    pub(super) fn enable_loss_tolerance(&mut self, client_id: ClientId) {
        // Disjoint field borrows: `self.terminal` / `self.synth` (RefCell
        // interior) vs `self.consumer_states` (via `get_mut`).
        let terminal = self.terminal.borrow();
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
            .prime_reference(&terminal, &mut state.acked_reference)
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

    /// Drop the per-consumer state for `client_id` if present
    /// (phux-q0e.2). Silent no-op if absent — matches the idempotency
    /// of `ServerState::detach`.
    pub(super) fn unregister_consumer(&mut self, client_id: ClientId) {
        // `HashMap::remove` returns the entry; dropping it frees the
        // per-consumer reference grid.
        let _ = self.consumer_states.remove(&client_id);
    }

    /// Handle an inbound `FRAME_ACK` from `client_id` carrying cumulative
    /// `seq` (phux-q0e.4, ADR-0018 addendum).
    ///
    /// Under the v0.1 emit-once model (phux-ia4) the per-consumer
    /// reference advances on *emit*, not on ack: a given change is shipped
    /// exactly once and the reference is committed before the frame goes
    /// out (see
    /// [`crate::grid::SnapshotSynthesizer::synthesize_against_reference`]).
    /// `FRAME_ACK` therefore no longer drives cache eviction; it tracks
    /// `last_acked_seq` for backpressure accounting (proto.md §8.2) and
    /// refreshes the informational cursor/mode capture. The loss-tolerance
    /// "re-diff against an older reference on a dropped frame" property is
    /// a future lossy-transport concern (ADR-0018), not wired on the
    /// reliable v0.1 transports.
    ///
    /// Per proto.md §8.2 acks are cumulative: an ack for `seq = N` implies
    /// all prior emissions up to `N`. Older / duplicate / out-of-order
    /// acks (`seq <= last_acked_seq`) are silently dropped.
    ///
    /// Silent no-op if `client_id` is not currently registered. This
    /// races cleanly against detach: the runtime may dispatch an
    /// in-flight ack just as the consumer is being torn down, and the
    /// ack should evaporate rather than recreate a dropped entry.
    ///
    /// Returns `true` when this ack folded a fresh RTT sample into the
    /// consumer's [`RttEstimator`] (phux-q0e.5) — the `run` loop uses that as
    /// a cue to recompute the shared adaptive tick cadence. `false` when no
    /// sample was produced (no matching emit instant, older/duplicate ack,
    /// or unregistered consumer).
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
        // Captured before the `&mut` borrow below: the global test override
        // that forces every consumer onto the tick.
        let force_all_consumers = self.consumer_tick_emits;
        let Some(consumer) = self.consumer_states.get_mut(&client_id) else {
            // Race against detach (or an ack for an unknown client). No
            // bookkeeping; no warning — this is a steady-state event,
            // not a misuse.
            trace!(
                ?client_id,
                seq, "FRAME_ACK for unregistered consumer; dropping"
            );
            return false;
        };
        // phux-38k6: only a tick-managed consumer's acks belong to this
        // per-consumer seq space. A raw (broadcast-pump) consumer acks the
        // pump's *local* seq, which is unrelated to this state's `next_seq` /
        // `emit_instants`; folding it in would set `last_acked_seq` from a
        // foreign counter and skew the RTT/backpressure accounting once the
        // consumer is (or becomes) state-sync. Drop it — the pump owns no
        // per-consumer state to update (phux-fseo made modes negotiable, so
        // this is now reachable, not just defensive).
        if !force_all_consumers && !consumer.wants_state_sync {
            trace!(
                ?client_id,
                seq, "FRAME_ACK for raw-broadcast consumer; not a tick ack, dropping"
            );
            return false;
        }
        if seq <= consumer.last_acked_seq {
            // Older or duplicate ack — acks are cumulative (proto.md
            // §8.2), so `seq <= last_acked_seq` carries no new information.
            trace!(
                ?client_id,
                seq,
                last_acked_seq = consumer.last_acked_seq,
                "FRAME_ACK older/duplicate; dropping",
            );
            return false;
        }
        consumer.last_acked_seq = seq;

        // Loss-tolerant reference advance (phux-v45.8, ADR-0042). A cumulative
        // ack for `seq` acknowledges every emission up to and including it, so
        // the consumer's acked reference advances to the grid snapshot of the
        // highest emitted `seq` this ack covers, and every pending snapshot at
        // or below `seq` is dropped. This is the eviction the emit-once path
        // (below, comment retained for contrast) deliberately does NOT do: on a
        // lossy leg the reference must trail the ack so a dropped frame re-diffs
        // against the last state the consumer provably has.
        if consumer.loss_tolerant {
            if let Some((&covered, _)) = consumer.pending_refs.range(..=seq).next_back()
                && let Some(snapshot) = consumer.pending_refs.remove(&covered)
            {
                consumer.acked_reference = snapshot;
            }
            // Keep only strictly-newer pending snapshots (still in flight).
            consumer.pending_refs = consumer.pending_refs.split_off(&seq.saturating_add(1));
        }

        // RTT sample (phux-q0e.5). Acks are cumulative, so `seq` acknowledges
        // every emission up to and including it. Find the emit instant for
        // the highest emitted seq that is `<= seq` (the most recent frame
        // this ack covers) and time it against now. Then prune every emit
        // instant `<= seq`: those frames are acked and can never produce a
        // future sample, so the map stays bounded by the in-flight window.
        let now = tokio::time::Instant::now();
        let rtt_sample = consumer
            .emit_instants
            .range(..=seq)
            .next_back()
            .map(|(_, &emitted_at)| now.saturating_duration_since(emitted_at));
        // `split_off(&(seq + 1))` keeps only the strictly-greater keys; the
        // returned (acked) half is dropped. `seq + 1` cannot overflow in
        // practice (u64 seq at the clamped cadence) but saturate for safety.
        let still_in_flight = consumer.emit_instants.split_off(&seq.saturating_add(1));
        consumer.emit_instants = still_in_flight;
        let sampled = if let Some(sample) = rtt_sample {
            consumer.rtt.observe(sample);
            trace!(
                ?client_id,
                seq,
                rtt_ms = sample.as_secs_f64() * 1000.0,
                srtt_ms = consumer.rtt.smoothed().map(|d| d.as_secs_f64() * 1000.0),
                "FRAME_ACK: RTT sample folded into EMA",
            );
            true
        } else {
            false
        };

        // Refresh the informational cursor/mode capture. Uses a one-shot
        // `RenderState` so it doesn't disturb the per-consumer reference.
        let terminal = self.terminal.borrow();
        let cursor_mode = match RenderState::new() {
            Ok(mut rs) => match rs.update(&terminal) {
                Ok(snapshot) => Some(LastAckedCursorMode::capture(&terminal, &snapshot)),
                Err(err) => {
                    warn!(
                        ?client_id,
                        seq,
                        error = %err,
                        "FRAME_ACK: cursor/mode capture update failed; keeping prior capture",
                    );
                    None
                }
            },
            Err(err) => {
                warn!(
                    ?client_id,
                    seq,
                    error = %err,
                    "FRAME_ACK: cursor/mode RenderState alloc failed; keeping prior capture",
                );
                None
            }
        };
        if let Some(cm) = cursor_mode {
            consumer.last_cursor_mode = cm;
        }

        trace!(
            ?client_id,
            seq, "FRAME_ACK applied: last_acked_seq advanced"
        );
        sampled
    }

    /// The shared adaptive tick interval for this actor: the minimum over
    /// every attached consumer's desired interval (phux-q0e.5).
    ///
    /// One `tokio::time::Interval` drives the whole pane, but RTT is
    /// per-consumer. Taking the *minimum* means the most-demanding (lowest
    /// half-RTT) consumer sets the cadence: a fast local peer keeps its 50 Hz
    /// feel even when sharing the pane with a slow satellite peer, and the
    /// slow peer simply sees more empty/short diffs per tick (harmless — the
    /// per-consumer reference advances only on a real delta). With no
    /// consumers, or none yet sampled, this is [`DEFAULT_TICK_INTERVAL`].
    pub(super) fn adaptive_tick_interval(&self) -> std::time::Duration {
        self.consumer_states
            .values()
            .map(|s| s.rtt.desired_tick_interval())
            .min()
            .unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    /// Rebuild the shared state-sync timer to fire at `desired` if it differs
    /// from the currently-armed `current` by more than [`TICK_RESET_DEADBAND`]
    /// (phux-q0e.5).
    ///
    /// The deadband keeps a steady RTT from churning the scheduler on every
    /// sub-millisecond EMA wobble. `tokio::time::Interval::reset_after`
    /// re-anchors only the next deadline; the recurring `period` is fixed at
    /// construction, so changing the cadence means rebuilding the interval.
    /// The first new tick is anchored one full `desired` out so the cadence
    /// change doesn't fire a tick immediately.
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
