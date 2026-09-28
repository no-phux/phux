//! Test-only [`TerminalActor`] methods and helpers shared by the
//! sibling test modules.

use super::*;

impl TerminalActor {
    /// The actor's current shared adaptive tick interval.
    #[cfg(test)]
    pub fn adaptive_tick_interval_for_test(&self) -> std::time::Duration {
        self.adaptive_tick_interval()
    }

    /// Drive `on_frame_ack`; `true` if it produced an RTT sample.
    #[cfg(test)]
    pub fn on_frame_ack_for_test(&mut self, client_id: ClientId, seq: u64) -> bool {
        self.on_frame_ack(client_id, seq)
    }

    /// Force tick emission for every consumer.
    #[cfg(test)]
    pub const fn enable_tick_emit_for_test(&mut self) {
        self.consumer_tick_emits = true;
    }

    /// Turn tick emission for every consumer off.
    #[cfg(test)]
    pub const fn disable_tick_emit_for_test(&mut self) {
        self.consumer_tick_emits = false;
    }

    /// Switch a registered consumer to the loss-tolerant model.
    #[cfg(test)]
    pub fn enable_loss_tolerance_for_test(&mut self, client_id: ClientId) {
        self.enable_loss_tolerance(client_id);
    }

    /// Backdate a consumer's emit instants by `by`, so the retransmit timer
    /// reads as elapsed.
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

    /// Write `bytes` and mark the tick dirty, like the PTY path (a direct
    /// `vt_write` would be skipped by the idle short-circuit).
    #[cfg(test)]
    pub fn vt_write_for_test(&mut self, bytes: &[u8]) {
        self.terminal.borrow_mut().vt_write(bytes);
        self.publish_input_snapshot();
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
    }

    /// Test-only: run one state-sync tick exactly as the `run` loop's tick
    /// arm would, without driving real time.
    #[cfg(test)]
    pub(crate) fn service_state_tick_for_test(&mut self) {
        self.service_state_tick();
    }

    /// Install in-memory PTY channels on a PTY-less actor: inject output on
    /// the returned sender, observe encoded input on the receiver.
    #[cfg(test)]
    pub(crate) fn install_test_pty_channels(
        &mut self,
    ) -> (mpsc::Sender<PtyEvent>, mpsc::Receiver<EncodedInputRequest>) {
        let (evt_tx, evt_rx) = mpsc::channel::<PtyEvent>(TEST_PTY_CHANNEL_DEPTH);
        let (writer_tx, writer_rx) = mpsc::channel::<EncodedInputRequest>(DEFAULT_INPUT_MAILBOX);
        self.pty_rx = Some(evt_rx);
        self.pty_tx = Some(writer_tx);
        (evt_tx, writer_rx)
    }
}

/// Depth of the test PTY channel: deep enough to pre-queue a whole burst
/// before spawning the actor.
pub(super) const TEST_PTY_CHANNEL_DEPTH: usize = 512;

/// Ceiling on joins of actors already told to stop. Not a performance
/// bound: it only turns a real hang into a failure, generously enough to
/// survive a saturated machine.
pub(super) const ACTOR_EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Poll granularity for drain loops, which run until
/// [`ACTOR_EXIT_DEADLINE`].
pub(super) const DRAIN_POLL_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// An outbound mailbox pair shaped like `AttachedClient::tx`; hold the
/// receiver so sends do not see a closed channel.
pub(super) fn dummy_outbound() -> (mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    mpsc::channel(16)
}

/// The viewport as right-trimmed rows, skipping wide-cell tails.
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

/// Drain every currently-queued `RESOURCE_OUTPUT` body from a consumer's
/// mailbox (its `seq` and bytes).
pub(super) fn drain_outputs(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(u64, Vec<u8>)> {
    let mut frames = Vec::new();
    while let Ok(Outbound::Frame(FrameKind::ResourceOutput { seq, bytes, .. })) = rx.try_recv() {
        frames.push((seq, bytes.to_vec()));
    }
    frames
}

/// Naive subsequence search for test assertions on VT byte streams.
pub(super) fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Scrollback bounds that set only the line count (bytes stay default).
#[cfg(test)]
pub(super) const fn test_scrollback(lines: u32) -> phux_config::ScrollbackLimits {
    phux_config::ScrollbackLimits::new(lines, phux_config::DEFAULT_HISTORY_BYTES)
}
