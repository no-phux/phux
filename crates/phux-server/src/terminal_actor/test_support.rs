//! Test-only [`TerminalActor`] methods and helpers shared by the
//! sibling test modules.

use super::*;

impl TerminalActor {
    /// Test-only: this actor's current shared adaptive tick interval
    /// (phux-q0e.5). Exposes [`Self::adaptive_tick_interval`] so tests can
    /// assert the cadence the `run` loop would arm without driving real time.
    #[cfg(test)]
    pub fn adaptive_tick_interval_for_test(&self) -> std::time::Duration {
        self.adaptive_tick_interval()
    }

    /// Test-only: drive `on_frame_ack` synchronously and report whether it
    /// produced a fresh RTT sample (phux-q0e.5). The `run` loop uses the
    /// return value to decide whether to re-arm the shared tick.
    #[cfg(test)]
    pub fn on_frame_ack_for_test(&mut self, client_id: ClientId, seq: u64) -> bool {
        self.on_frame_ack(client_id, seq)
    }

    /// Test-only: enable the per-consumer tick emission gate
    /// (`consumer_tick_emits`). Production defaults this OFF for human
    /// attach; this setter lets state-sync tests opt into the synthesized
    /// output path explicitly.
    #[cfg(test)]
    pub const fn enable_tick_emit_for_test(&mut self) {
        self.consumer_tick_emits = true;
    }

    /// Test-only: disable the per-consumer tick emission gate so the
    /// `tick_emit`-stays-silent path can be asserted locally, independent
    /// of the production default.
    #[cfg(test)]
    pub const fn disable_tick_emit_for_test(&mut self) {
        self.consumer_tick_emits = false;
    }

    /// Test-only: flip an already-registered state-sync consumer to the
    /// advance-on-ack loss-tolerant model (phux-v45.8), mirroring what the
    /// runtime does for a forwarded/lossy-leg consumer right after attach.
    #[cfg(test)]
    pub fn enable_loss_tolerance_for_test(&mut self, client_id: ClientId) {
        self.enable_loss_tolerance(client_id);
    }

    /// Test-only: backdate this consumer's emit instants by `by` so the
    /// loss-tolerant retransmit timer reads as elapsed on the next tick
    /// (phux-v45.8), letting a retransmit be exercised without sleeping real
    /// time. Saturates at the process epoch rather than underflowing.
    #[cfg(test)]
    pub fn backdate_emit_instants_for_test(
        &mut self,
        client_id: ClientId,
        by: std::time::Duration,
    ) {
        if let Some(state) = self.consumer_states.get_mut(&client_id) {
            let now = tokio::time::Instant::now();
            let past = now.checked_sub(by).unwrap_or(now);
            for instant in state.emit_instants.values_mut() {
                *instant = past;
            }
        }
    }

    /// Test-only: write `bytes` into the actor's `Terminal` and mark the
    /// per-tick dirty flag, mirroring the production PTY-byte path so the
    /// phux-4l0 idle short-circuit sees the mutation. Tests must use this
    /// rather than poking `terminal.borrow_mut().vt_write` directly, or
    /// the next `tick_emit` would short-circuit and skip the write.
    #[cfg(test)]
    pub fn vt_write_for_test(&mut self, bytes: &[u8]) {
        self.terminal.borrow_mut().vt_write(bytes);
        self.publish_input_snapshot();
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
    }

    /// Test-only: install in-memory PTY channels on a no-PTY actor so a
    /// test can inject a PTY-output burst (the returned
    /// [`mpsc::UnboundedSender<PtyEvent>`]) and observe the encoded input
    /// the actor forwards toward the PTY writer thread (the returned
    /// [`mpsc::Receiver<EncodedInputRequest>`]). Faithful to production
    /// wiring: queued output is consumed by `vt_write`; serviced input
    /// surfaces on the writer receiver. `pty` stays `None` — the run loop
    /// only reads `pty_tx` for input forwarding and `pty` for cwd/EOF,
    /// neither of which this seam exercises.
    #[cfg(test)]
    pub(crate) fn install_test_pty_channels(
        &mut self,
    ) -> (
        mpsc::UnboundedSender<PtyEvent>,
        mpsc::Receiver<EncodedInputRequest>,
    ) {
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<PtyEvent>();
        let (writer_tx, writer_rx) = mpsc::channel::<EncodedInputRequest>(DEFAULT_INPUT_MAILBOX);
        self.pty_rx = Some(evt_rx);
        self.pty_tx = Some(writer_tx);
        (evt_tx, writer_rx)
    }
}

/// Ceiling for "this task has already been told to stop, so joining it
/// should return immediately" waits.
///
/// The number is deliberately NOT load-bearing. Nothing here measures how
/// fast an actor shuts down — the assertion is that it shuts down at all,
/// and every one of these joins resolves in single-digit milliseconds on
/// an idle box. What the timeout buys is a bounded failure for a genuine
/// hang instead of a wedged test binary.
///
/// It used to be 500ms, which made it a latent flake: these tests share a
/// machine with the rest of the suite, and a current-thread runtime that
/// loses its core for half a second turns "the actor exited" into "the
/// actor did not exit within 500ms". Two of them (phux-br1f:
/// `rapid_resizes_coalesce_into_one_resync_snapshot` and
/// `parent_token_cancel_cascades_to_pane_actor`) failed exactly that way
/// on a saturated laptop and passed in ~0.1s when re-run alone. A
/// suite that cries wolf gets ignored, which is how a real regression
/// ships.
///
/// Raising it does not weaken anything: a real hang still fails, just 30s
/// later, and the panic message names the actor that would not exit.
pub(super) const ACTOR_EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long one `recv` in the drain loops below waits before the loop
/// re-checks its predicate (and, where relevant, re-pokes the PTY).
///
/// A granularity knob, not a bound: the loops that use it run until
/// `ACTOR_EXIT_DEADLINE`. They used to run for a fixed ITERATION count
/// instead, which is a wall-clock deadline wearing a disguise — `0..32`
/// ticks of 100ms is a 3.2s budget that a starved runtime blows through
/// without the actor being at fault, and it also caps how many frames the
/// loop may consume, so a chatty actor could exhaust it before the frame
/// under test even arrived.
pub(super) const DRAIN_POLL_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Test helper: build a throwaway outbound mailbox + receiver pair
/// shaped like the production [`crate::state::AttachedClient::tx`].
/// The receiver is returned so callers can hold it open (otherwise
/// the actor's `try_send` would see a closed channel).
pub(super) fn dummy_outbound() -> (mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    mpsc::channel(16)
}

/// Render a `Terminal`'s viewport into right-trimmed rows, skipping
/// wide-cell tails — enough to assert grid equivalence between a
/// state-sync mirror and the canonical after applying deltas.
pub(super) fn render_viewport(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    use libghostty_vt::render::{CellIterator, RowIterator};
    use libghostty_vt::screen::CellWide;
    let mut rs = RenderState::new().expect("RenderState::new");
    let snap = rs.update(t).expect("update");
    let rows_n = snap.rows().expect("rows");
    let mut rows = RowIterator::new().expect("RowIterator::new");
    let mut cells = CellIterator::new().expect("CellIterator::new");
    let mut row_iter = rows.update(&snap).expect("row update");
    let mut out: Vec<String> = Vec::with_capacity(usize::from(rows_n));
    let mut i: u16 = 0;
    while let Some(row) = row_iter.next() {
        if i >= rows_n {
            break;
        }
        let mut line = String::new();
        let mut cell_iter = cells.update(row).expect("cell update");
        while let Some(cell) = cell_iter.next() {
            if matches!(
                cell.raw_cell().expect("rc").wide().expect("wide"),
                CellWide::SpacerTail
            ) {
                continue;
            }
            let g = cell.graphemes().expect("graphemes");
            if g.is_empty() {
                line.push(' ');
            } else {
                line.extend(g);
            }
        }
        out.push(line.trim_end().to_owned());
        i += 1;
    }
    out
}

/// Drain every currently-queued `TERMINAL_OUTPUT` body from a consumer's
/// mailbox (its `seq` and bytes).
pub(super) fn drain_outputs(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(u64, Vec<u8>)> {
    let mut frames = Vec::new();
    while let Ok(Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. })) = rx.try_recv() {
        frames.push((seq, bytes.to_vec()));
    }
    frames
}

/// Naive subsequence search for test assertions on VT byte streams.
pub(super) fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
