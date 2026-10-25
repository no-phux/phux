use super::{
    ClientId, ConsumerSyncState, FrameKind, MAX_EMIT_INSTANTS, Outbound, SnapshotSynthesizer,
    TerminalActor, TickEmitWalk, TickOutcome, TickRender, debug, tick, trace, warn,
};

impl TerminalActor {
    /// One tick of the state-sync emitter (ADR-0018).
    ///
    /// For each tick-managed consumer: diff against its own reference (not
    /// libghostty's shared dirty bits), skip an empty body, else stamp the
    /// next `seq` and ship. Errors skip that consumer only. The reference
    /// advances on emit, so each change ships once.
    pub(in crate::resource::terminal) fn tick_emit(&mut self) {
        // Debug-level span (free when disabled) carrying consumer count and
        // dirtiness; `emitted`/`total_out_bytes` are recorded at the end.
        let tick_span = tracing::debug_span!(
            "tick_emit",
            consumer_count = self.consumer_states.len(),
            dirty = self.terminal_dirty_since_tick,
            emitted = tracing::field::Empty,
            total_out_bytes = tracing::field::Empty,
        )
        .entered();

        let Some(force_all_consumers) = self.tick_emit_audience() else {
            return;
        };
        let Some(mutated) = self.take_tick_mutation() else {
            return;
        };
        let Some(walk) = self.emit_ready_tick(force_all_consumers, mutated) else {
            return;
        };
        tick_span.record("emitted", walk.emitted);
        tick_span.record("total_out_bytes", walk.total_out_bytes);
        crate::perf::TICK_EMIT.record_elapsed(walk.started);
        for client_id in walk.closed {
            self.consumer_states.remove(&client_id);
        }
    }

    /// Whether this tick has anyone to serve, plus the test gate that forces
    /// every consumer onto the tick. Only `StateSync` consumers are
    /// tick-managed; raw consumers get the broadcast pump.
    pub(super) fn tick_emit_audience(&self) -> Option<bool> {
        let force_all_consumers = self.consumer_tick_emits;
        if !force_all_consumers && !self.consumer_states.values().any(|s| s.wants_state_sync) {
            // No tick-managed consumer: nothing to emit (dirty flag untouched).
            return None;
        }
        Some(force_all_consumers)
    }

    /// Take the "mutated since last tick" flag. On a clean terminal the walk
    /// is skipped unless some consumer still needs one.
    pub(super) fn take_tick_mutation(&mut self) -> Option<bool> {
        let mutated = self.terminal_dirty_since_tick;
        self.terminal_dirty_since_tick = false;
        if !mutated && !self.consumer_states.values().any(must_walk_when_clean) {
            return None;
        }
        Some(mutated)
    }

    /// Render once and walk every consumer. `None` when `prepare_tick`
    /// fails. Timed here so cheap skipped ticks do not swamp the histogram.
    pub(super) fn emit_ready_tick(
        &mut self,
        force_all_consumers: bool,
        mutated: bool,
    ) -> Option<TickEmitWalk> {
        let started = std::time::Instant::now();

        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            // Terminal on loan to a capture; the resync after repaints.
            trace!("tick skipped: canonical terminal is on loan to a capture");
            return None;
        };
        let mut synth = self.synth.borrow_mut();
        // Render once; each consumer only diffs against the result.
        let render = match synth.prepare_tick(terminal) {
            Ok((cols, rows, live_cm)) => TickRender {
                cols,
                rows,
                live_cm,
            },
            Err(err) => {
                warn!(error = %err, "state-sync tick: prepare_tick failed; skipping tick");
                return None;
            }
        };
        // Consumers with a closed mailbox are reaped after the loop.
        let mut closed: Vec<ClientId> = Vec::new();
        let mut emitted: u64 = 0;
        let mut total_out_bytes: usize = 0;
        for (client_id, state) in &mut self.consumer_states {
            match Self::emit_consumer_tick(
                *client_id,
                state,
                &synth,
                render,
                mutated,
                force_all_consumers,
            ) {
                TickOutcome::Skipped => {}
                TickOutcome::Closed => closed.push(*client_id),
                TickOutcome::Emitted(out_bytes) => {
                    emitted += 1;
                    total_out_bytes += out_bytes;
                }
            }
        }
        drop(synth);
        drop(canonical);
        Some(TickEmitWalk {
            emitted,
            total_out_bytes,
            closed,
            started,
        })
    }

    /// Serve one consumer: reserve its outbound slot, diff, ship.
    pub(super) fn emit_consumer_tick(
        client_id: ClientId,
        state: &mut ConsumerSyncState,
        synth: &SnapshotSynthesizer<'_>,
        render: TickRender,
        mutated: bool,
        force_all_consumers: bool,
    ) -> TickOutcome {
        // Raw consumers are served by the broadcast pump.
        if !force_all_consumers && !state.wants_state_sync {
            return TickOutcome::Skipped;
        }
        if !*state.live_gate.borrow() {
            return TickOutcome::Skipped;
        }
        // Captured before the reset: a delta held back by backpressure.
        let was_behind = state.behind;
        state.needs_initial_emit = false;
        // Reserve before diffing: the diff advances the reference, so a
        // delta that then failed to send would be lost for good. A full
        // mailbox skips this consumer untouched; a closed one is reaped.
        let permit = match state.outbound.try_reserve() {
            Ok(permit) => permit,
            Err(tokio::sync::mpsc::error::TrySendError::Full(())) => {
                // Retry next tick without advancing; `behind` keeps the walk
                // alive even if the grid goes clean meanwhile.
                state.behind = true;
                crate::perf::CONSUMER_MAILBOX_FULL.incr();
                if let Some(suppressed) = crate::perf::MAILBOX_FULL_WARN.admit() {
                    warn!(
                        ?client_id,
                        wire_terminal_id = state.wire_terminal_id,
                        suppressed,
                        "state-sync tick: consumer mailbox full; skipping (reference held, retries next tick)",
                    );
                }
                return TickOutcome::Skipped;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => {
                // The detach may have been dropped; reap now.
                crate::perf::CONSUMER_REAPED.incr();
                debug!(
                    ?client_id,
                    wire_terminal_id = state.wire_terminal_id,
                    "state-sync tick: consumer mailbox closed; reaping entry",
                );
                return TickOutcome::Closed;
            }
        };
        // Holding a permit: this consumer is served this tick.
        state.behind = false;
        let _synth_span = tracing::debug_span!(
            "synthesize",
            ?client_id,
            wire_terminal_id = state.wire_terminal_id,
        )
        .entered();
        let synth_started = std::time::Instant::now();
        let bytes = consumer_delta(
            synth,
            render,
            state.loss_tolerant,
            &state.acked_reference,
            &mut state.reference,
        );
        crate::perf::TICK_SYNTH.record_elapsed(synth_started);
        if bytes.is_empty() {
            // Unchanged; the permit drops unused.
            return TickOutcome::Skipped;
        }
        if state.loss_tolerant && holds_loss_tolerant_delta(state, mutated, was_behind) {
            return TickOutcome::Skipped;
        }
        let seq = state.next_seq;
        let out_bytes = bytes.len();
        crate::perf::TICK_OUT_BYTES.record_len(out_bytes);
        state.next_seq = state.next_seq.wrapping_add(1);
        let frame = FrameKind::ResourceOutput {
            terminal_id: phux_protocol::ids::ResourceId::local(state.wire_terminal_id),
            stream_id: state.stream_id,
            bootstrap_id: state.bootstrap_id,
            seq,
            bytes: bytes.into(),
        };
        // Infallible with a reserved permit.
        permit.send(Outbound::Frame(frame));
        record_emit_instant(&mut state.emit_instants, seq);
        // Loss-tolerant: remember the grid this `seq` shipped so a later
        // ack can advance the acked reference to it (bounded).
        if state.loss_tolerant {
            let snapshot = synth.snapshot_tick_reference(render.cols, render.rows, render.live_cm);
            record_pending_ref(&mut state.pending_refs, seq, snapshot);
        }
        trace!(
            ?client_id,
            wire_terminal_id = state.wire_terminal_id,
            seq,
            out_bytes,
            "state-sync tick: RESOURCE_OUTPUT emitted",
        );
        TickOutcome::Emitted(out_bytes)
    }
}
/// One consumer's delta against the just-rendered tick. Loss-tolerant
/// consumers diff against their last-acked reference (not advanced on emit),
/// so a dropped frame self-heals; others use emit-once `diff_consumer`.
fn consumer_delta(
    synth: &SnapshotSynthesizer<'_>,
    render: TickRender,
    loss_tolerant: bool,
    acked_reference: &crate::grid::ConsumerReference,
    reference: &mut crate::grid::ConsumerReference,
) -> Vec<u8> {
    if loss_tolerant {
        synth.diff_against_base(render.cols, render.rows, render.live_cm, acked_reference)
    } else {
        synth
            .diff_consumer(render.cols, render.rows, render.live_cm, reference)
            .bytes
    }
}

/// Whether a consumer must be walked on a clean terminal: it has never been
/// served (`needs_initial_emit`), a backpressured delta is owed (`behind`),
/// or a loss-tolerant frame is still un-acked.
pub(super) fn must_walk_when_clean(state: &ConsumerSyncState) -> bool {
    state.needs_initial_emit || state.behind || !state.pending_refs.is_empty()
}

/// Loss-tolerant emit gate: the diff stays non-empty while a frame is
/// un-acked, so ship only on new content, a post-backpressure flush, or a
/// due retransmit.
fn holds_loss_tolerant_delta(state: &ConsumerSyncState, mutated: bool, was_behind: bool) -> bool {
    let now = tokio::time::Instant::now();
    let retransmit_due = !state.pending_refs.is_empty()
        && state.emit_instants.values().next_back().is_none_or(|last| {
            now.saturating_duration_since(*last) >= tick::retransmit_timeout(state.rtt.smoothed())
        });
    !mutated && !was_behind && !retransmit_due
}

/// Record the emit instant for `seq`, for RTT on its `FRAME_ACK`. Oldest
/// entries are evicted past [`MAX_EMIT_INSTANTS`] so a consumer that never
/// acks cannot grow the map.
fn record_emit_instant(
    emit_instants: &mut std::collections::BTreeMap<u64, tokio::time::Instant>,
    seq: u64,
) {
    emit_instants.insert(seq, tokio::time::Instant::now());
    while emit_instants.len() > MAX_EMIT_INSTANTS {
        emit_instants.pop_first();
    }
}

/// Retain the grid a loss-tolerant `seq` shipped, bounded like emit instants.
fn record_pending_ref(
    pending_refs: &mut std::collections::BTreeMap<u64, crate::grid::ConsumerReference>,
    seq: u64,
    snapshot: crate::grid::ConsumerReference,
) {
    pending_refs.insert(seq, snapshot);
    while pending_refs.len() > MAX_EMIT_INSTANTS {
        pending_refs.pop_first();
    }
}
