//! Snapshot synthesis, input encoding, resize handling, and PTY
//! lifecycle plumbing for [`TerminalActor`].

use super::{
    Bytes, EncodedInputRequest, InputEncoderSnapshot, PANE_KILL_POLL, PANE_KILL_REAP_BUDGET,
    PaneOutput, PasteOutcome, PtyOwned, PtySize, ResyncAudience, ResyncReason, SizeReportSize,
    SnapshotBytes, TerminalActor, TerminalInput, WriteCompletion, debug, error, exit_outcome, mpsc,
    oneshot, trace, warn,
};

impl TerminalActor {
    /// Synthesize a snapshot of the current `Terminal` (test entry point).
    pub(super) fn synthesize(&self) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        self.synthesize_with_scrollback(None)
    }

    /// Synthesize an ATTACH snapshot, optionally priming scrollback (see
    /// [`crate::grid::SnapshotSynthesizer::synthesize_with_scrollback`]).
    pub(super) fn synthesize_with_scrollback(
        &self,
        scrollback: Option<u32>,
    ) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            return Err(crate::grid::SynthesisError::TerminalUnavailable);
        };
        // A full snapshot uses a fresh render state; a shared borrow suffices.
        let synth = self.synth.borrow();
        synth.synthesize_with_scrollback(terminal, scrollback)
    }

    pub(super) fn synthesize_with_scrollback_bounded(
        &self,
        scrollback: Option<u32>,
        max_bytes: usize,
    ) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            return Err(crate::grid::SynthesisError::TerminalUnavailable);
        };
        let synth = self.synth.borrow();
        synth.synthesize_with_scrollback_bounded(terminal, scrollback, max_bytes)
    }

    /// Project the grid into a [`phux_core::screen::ScreenState`] for
    /// `GET_SCREEN`, stamping `pane`. `format` also renders through
    /// libghostty's Formatter (`0` none, `1` HTML, `2` VT).
    ///
    /// A render failure is non-fatal (`rendered_error` says why), except
    /// [`SynthesisError::RenderBudgetExceeded`], which propagates so the
    /// caller refuses with `RESOURCE_EXHAUSTED`. With a rendering the plain
    /// text fields are omitted (the capture carries the same content).
    pub(super) fn screen_state(
        &self,
        pane: u32,
        scrollback: Option<u32>,
        cells: bool,
        format: u8,
    ) -> Result<phux_core::screen::ScreenState, crate::grid::SynthesisError> {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            return Err(crate::grid::SynthesisError::TerminalUnavailable);
        };
        // Exclusive: the projection walks the tick's pooled render state.
        let mut synth = self.synth.borrow_mut();
        let mut screen = synth.screen_state_with_scrollback(terminal, pane, scrollback, cells)?;
        match synth.render_screen(terminal, scrollback, format) {
            Ok(rendered) => screen.rendered = rendered,
            Err(err @ crate::grid::SynthesisError::RenderBudgetExceeded { .. }) => return Err(err),
            Err(err) => {
                warn!(
                    error = %err,
                    format,
                    "GET_SCREEN rendered-capture failed; keeping the plain projection"
                );
                screen.rendered_error = Some(err.to_string());
            }
        }
        let selector = format & phux_protocol::wire::frame::GET_SCREEN_FORMAT_SELECTOR_MASK;
        if selector != 0 {
            screen.lines = Vec::new();
            screen.scrollback = Vec::new();
            screen.soft_wrap = None;
            screen.truncated = false;
            screen.truncated_reason = None;
        }
        Ok(screen)
    }

    /// Publish the complete terminal-derived encoder state after a terminal
    /// mutation. Capture failures retain the previous good snapshot.
    pub(super) fn publish_input_snapshot(&self) {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            // On loan to a capture; the resync after its return covers it.
            trace!("input snapshot skipped: canonical terminal is on loan");
            return;
        };
        match InputEncoderSnapshot::capture(terminal, self.cell_px) {
            Ok(snapshot) => {
                self.input_snapshot_tx.send_replace(snapshot);
            }
            Err(err) => warn!(error = %err, "input encoder snapshot capture failed"),
        }
    }

    /// Was the PTY quiet long enough that the next output can be attributed
    /// to this input (not to an already-streaming job)?
    fn pane_quiet_for_echo(&self) -> bool {
        self.last_output_at
            .get()
            .is_none_or(|at| at.elapsed() >= crate::perf::ECHO_QUIET_WINDOW)
    }

    /// Encode a [`TerminalInput`] into PTY bytes. `Ok(None)` means
    /// deliberately dropped (focus reports off, a rejected paste); `Err` is an
    /// encoder failure the caller logs.
    pub(super) fn encode_input(
        &self,
        input: &TerminalInput,
    ) -> Result<Option<Vec<u8>>, libghostty_vt::Error> {
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            // Encoding reads terminal modes, so drop while on loan; the gated
            // input arms keep this off the production path.
            trace!("input dropped: canonical terminal is on loan to a capture");
            return Ok(None);
        };
        match input {
            TerminalInput::Key(event) => {
                let mut enc = self.key_enc.borrow_mut();
                let bytes = enc.encode(event, terminal)?;
                Ok(Some(bytes.to_vec()))
            }
            TerminalInput::Mouse(event) => {
                let mut enc = self.mouse_enc.borrow_mut();
                let bytes = enc.encode(event, terminal, self.cell_px)?;
                Ok(Some(bytes.to_vec()))
            }
            TerminalInput::Focus(event) => {
                let mut enc = self.focus_enc.borrow_mut();
                let bytes = enc.encode(*event, terminal)?;
                Ok(bytes.map(<[u8]>::to_vec))
            }
            TerminalInput::Paste(event) => {
                let mut enc = self.paste_enc.borrow_mut();
                match enc.encode(event, terminal)? {
                    PasteOutcome::Encoded(bytes) => Ok(Some(bytes.to_vec())),
                    PasteOutcome::Rejected => Ok(None),
                }
            }
        }
    }

    /// Encode one input event and forward it to the PTY writer. Failures
    /// log and drop; one bad event must not kill the actor.
    pub(super) fn service_input(&self, input: &TerminalInput) {
        // Log every outcome: ROUTE_INPUT acks regardless, so this is the only
        // witness when a key vanishes.
        match self.encode_input(input) {
            Ok(Some(bytes)) => {
                if bytes.is_empty() {
                    debug!(?input, "input encoded to zero bytes; nothing to write");
                    return;
                }
                self.service_encoded_input(EncodedInputRequest::legacy_probe(
                    bytes,
                    super::echo_probe_for(input),
                ));
            }
            Ok(None) => {
                debug!(?input, "input gated/dropped by encoder");
            }
            Err(err) => {
                warn!(error = %err, "input encode failed; dropping event");
            }
        }
    }

    /// Forward lane-encoded bytes to the PTY writer. Every discard here
    /// happens before any `write(2)`, so an acknowledged request reports
    /// [`WriteCompletion::NotWritten`] explicitly.
    pub(super) fn service_encoded_input(&self, request: EncodedInputRequest) {
        if request.bytes.is_empty() && request.completion.is_none() {
            return;
        }
        let echo_probe = request.echo_probe;
        let len = request.bytes.len();
        let Some(tx) = self.pty_tx.as_ref() else {
            debug!("no PTY; encoded input discarded");
            if let Some(completion) = request.completion {
                completion.complete(WriteCompletion::NotWritten);
            }
            return;
        };
        match super::try_send_to_writer(tx, request) {
            Ok(()) => {
                crate::perf::INPUT_EVENTS.incr();
                if echo_probe && self.pane_quiet_for_echo() {
                    self.last_input_at.set(Some(std::time::Instant::now()));
                }
                debug!(len, "input queued to PTY writer");
            }
            // Credited input always finds a slot (ADR-0144): only a reply or
            // uncredited no-lane input can land here.
            Err(mpsc::error::TrySendError::Full(request)) => {
                warn!(
                    len,
                    credited = request.credit.is_some(),
                    "PTY writer queue full; dropping input"
                );
                if let Some(completion) = request.completion {
                    completion.complete(WriteCompletion::NotWritten);
                }
            }
            // The writer thread is gone: input is dead while output lives on.
            Err(mpsc::error::TrySendError::Closed(request)) => {
                error!(len, "PTY writer channel closed; pane input is dead");
                if let Some(completion) = request.completion {
                    completion.complete(WriteCompletion::NotWritten);
                }
            }
        }
    }

    /// Apply a resize to the `Terminal` and the PTY winsize, returning
    /// whether anything moved. An unchanged geometry earns no resync (that
    /// would rotate the bootstrap generation for nothing).
    pub(super) fn handle_resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_px: Option<(u16, u16)>,
    ) -> bool {
        // libghostty rejects zero dimensions; clamp to 1 like the ATTACH
        // path (SPEC §10.5).
        let cols = cols.max(1);
        let rows = rows.max(1);
        // The settled geometry repeated is a no-op; it must not invalidate
        // native history cursors.
        if cols == self.cols && rows == self.rows && cell_px.is_none_or(|cell| cell == self.cell_px)
        {
            self.apply_pty_resize();
            return false;
        }
        // Sticky cell size: see the `ResizeRequest::cell_px` doc.
        let (cell_w, cell_h) = cell_px.unwrap_or(self.cell_px);

        // Pixel dimensions are `cells x cell size`, always nonzero.
        let applied = {
            let mut term = self.terminal.borrow_mut();
            if let Err(err) = term.resize(cols, rows, u32::from(cell_w), u32::from(cell_h)) {
                warn!(?err, cols, rows, "terminal resize failed");
                return false;
            }
            // Cache what libghostty settled on, not the request.
            term.try_terminal().map_or((cols, rows), |t| {
                (t.cols().unwrap_or(cols), t.rows().unwrap_or(rows))
            })
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            self.reflow_tombstoned = self
                .invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);
        }
        self.cols = applied.0;
        self.rows = applied.1;
        self.cell_px = (cell_w, cell_h);
        self.size_report.set(SizeReportSize {
            rows: applied.1,
            columns: applied.0,
            cell_width: u32::from(cell_w),
            cell_height: u32::from(cell_h),
        });
        self.publish_input_snapshot();
        // A reflow rebuilds every consumer reference and may move detector
        // regions: force both ticks to rescan.
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
        self.pty_resize_pending = true;
        self.apply_pty_resize();
        true
    }

    /// A failed kernel resize is not a settled no-op. A later identical
    /// request retries only the ioctl, without invalidating history again.
    fn apply_pty_resize(&mut self) {
        if !self.pty_resize_pending {
            return;
        }
        let Some(pty) = &self.pty else {
            self.pty_resize_pending = false;
            return;
        };
        let (cell_w, cell_h) = self.cell_px;
        let size = PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: self.cols.saturating_mul(cell_w),
            pixel_height: self.rows.saturating_mul(cell_h),
        };
        let Ok(master) = pty.master.lock() else {
            warn!("pty resize master lock poisoned");
            return;
        };
        match master.resize(size) {
            Ok(()) => self.pty_resize_pending = false,
            Err(err) => warn!(
                ?err,
                cols = self.cols,
                rows = self.rows,
                "pty resize ioctl failed"
            ),
        }
    }

    /// Broadcast a full synthesized snapshot as an in-band
    /// [`PaneOutput::Resync`].
    ///
    /// Used after a reflow (server and client reflow independently and can
    /// diverge, and live output never re-sends history) and for pumps that
    /// lagged past the broadcast buffer. The replay opens with a reset, so it
    /// cleanly supersedes the client mirror. `audience` picks which pumps
    /// replace their generation; it is still one ordered broadcast item, so
    /// it lands after every chunk those pumps already saw.
    pub(super) fn broadcast_resync(&self, reason: ResyncReason, audience: ResyncAudience) {
        // No attached pumps (the actor's seed receiver was dropped).
        if self.core.output_tx.receiver_count() == 0 {
            return;
        }
        match self.synthesize() {
            Ok(snap) => {
                debug!(
                    bytes = snap.bytes.len(),
                    "resize resync: snapshot broadcast"
                );
                // Send errors are benign. `Resync` carries the settled dims
                // so the mirror resizes before applying the replay.
                let _ = self.core.output_tx.send(PaneOutput::Resync {
                    cols: self.cols,
                    rows: self.rows,
                    reason,
                    audience,
                    base_seq: self.core.seq(),
                    bytes: Bytes::from(snap.bytes),
                });
            }
            Err(err) => {
                warn!(
                    error = %err,
                    "resize resync: snapshot synthesis failed; clients recover on next output",
                );
            }
        }
    }

    /// Reap the child on PTY EOF if it has exited, returning its
    /// [`ExitOutcome`](phux_core::process::ExitOutcome) (code, signal, or
    /// unknown). A child still running is left to the shutdown path.
    pub(super) fn reap_child_if_any(&mut self) -> phux_core::process::ExitOutcome {
        use phux_core::process::ExitOutcome;
        let Some(pty) = self.pty.as_mut() else {
            return ExitOutcome::UNKNOWN;
        };
        // EOF can precede the child becoming waitable, so retry briefly. The
        // blocking sleep is once per pane and tiny; a daemonizer that keeps
        // running exhausts it and reports unknown.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        loop {
            match pty.child.try_wait() {
                Ok(Some(status)) => {
                    debug!(?status, "child reaped on PTY EOF");
                    return exit_outcome(&status);
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(None) => {
                    trace!("PTY EOF but child still alive — leaving to shutdown path");
                    return ExitOutcome::UNKNOWN;
                }
                Err(err) => {
                    debug!(?err, "child try_wait failed on PTY EOF");
                    return ExitOutcome::UNKNOWN;
                }
            }
        }
    }

    /// React to PTY EOF: stop reading, reap the child, record the exit
    /// facet. The actor stays alive for late `SnapshotRequest`s and orderly
    /// shutdown; the run loop fires `exit_notify` after flushing gap
    /// resyncs. The runtime decides whether to close the pane or respawn a
    /// shell.
    pub(super) fn handle_pty_eof(&mut self) {
        debug!("PTY EOF; recording exit and keeping actor alive for a final resync flush");
        self.pty_rx = None;
        let exit = self.reap_child_if_any();
        self.record_exit(exit);
    }

    /// Spawn a replacement child in this same Terminal after PTY EOF.
    pub(super) fn replace_child(
        &mut self,
        mut cmd: portable_pty::CommandBuilder,
    ) -> Result<oneshot::Receiver<phux_core::process::ExitOutcome>, String> {
        self.apply_replacement_cwd(&mut cmd);
        self.release_pty_after_exit();
        self.reset_for_replacement();
        let spawned = super::spawn_pty(cmd, self.cols, self.rows).map_err(|err| err.to_string())?;
        self.install_replacement_pty(spawned)?;
        Ok(self.core.arm_exit_notify())
    }

    fn apply_replacement_cwd(&self, cmd: &mut portable_pty::CommandBuilder) {
        let cwd = self.last_known_cwd.borrow();
        if !cwd.is_empty() {
            cmd.cwd(cwd.as_str());
        }
    }

    pub(super) fn reset_for_replacement(&mut self) {
        self.exit = None;
        self.lifecycle = super::ResourceLifecycle::Running;
        self.osc133 = super::osc133::Osc133Scanner::new();
        self.prompt = super::osc133::PromptTracker::default();
        self.last_title.clear();
        self.last_progress.clear();
        self.in_output_burst = false;
        self.output_since_idle_tick = false;
        // Land in-flight native cuts before the reset: a pending bootstrap
        // holds the terminal, and a client may be attaching right now. The
        // everyone-resync from `install_replacement_pty` covers the pumps.
        self.land_native_cuts();
        self.terminal.borrow_mut().reset_for_new_child();
    }

    /// Fail any in-flight native bootstrap and tombstone every cursor,
    /// returning the terminal to the manager. The teardown paths run outside
    /// the `!bootstrap_pending` guards, so they land the cut instead of
    /// touching a loaned terminal. No-op without the native engine.
    pub(super) fn land_native_cuts(&mut self) {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            let _ = self
                .invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Other);
        }
    }

    fn install_replacement_pty(
        &mut self,
        spawned: (
            mpsc::Receiver<super::PtyEvent>,
            mpsc::Sender<super::EncodedInputRequest>,
            super::PtyOwned,
        ),
    ) -> Result<(), String> {
        let (pty_rx, pty_tx, pty) = spawned;
        self.pty_rx = Some(pty_rx);
        self.pty_tx = Some(pty_tx);
        self.pty = Some(pty);
        let facts = super::process_facet::ChildFacts::capture(self.pty.as_ref());
        self.child_start_ms = facts.start_ms;
        self.released_child_pid = None;
        if !facts.cwd.is_empty() {
            self.last_known_cwd.borrow_mut().clone_from(&facts.cwd);
            self.cwd_announced.set(false);
        }
        self.terminal
            .borrow_mut()
            .reinstall_pty_write(&self.size_report, self.pty_tx.as_ref())
            .map_err(|err| err.to_string())?;
        self.publish_input_snapshot();
        self.broadcast_resync(ResyncReason::Resize, super::ResyncAudience::Everyone);
        Ok(())
    }

    /// Release a retained pane's PTY after its process exited (ADR-0124);
    /// the grid stays. A child EOF could not reap goes to a reaper thread,
    /// decided by the recorded exit, not a second `try_wait` on a maybe
    /// recycled pid.
    pub(super) fn release_pty_after_exit(&mut self) {
        self.pty_rx = None;
        drop(self.pty_tx.take());
        let Some(mut pty) = self.pty.take() else {
            return;
        };
        let pid = pty.child.process_id();
        self.released_child_pid = pid.and_then(|pid| i32::try_from(pid).ok());
        let reaped = self
            .exit
            .as_ref()
            .is_some_and(|exit| exit.status.is_some() || exit.signal.is_some());
        if !reaped {
            spawn_detached_reaper(pid);
        }
        // Both threads end on their own; detach rather than join.
        drop(pty.reader_thread.take());
        drop(pty.writer_thread.take());
        drop(pty);
    }

    /// Tear down the PTY: gracefully stop a live child, drop the master, and
    /// join the bridge threads (bounded). Errors are logged.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    pub(super) async fn shutdown_pty(&mut self) {
        let Some(mut pty) = self.pty.take() else {
            return;
        };
        // Close the PTY receiver first. The actor stops draining it now, and
        // a full queue would park the reader, leaving the master unread and
        // wedging a child mid-flush in `write(2)`. Dropped, the reader
        // switches to discarding until EOF.
        self.pty_rx = None;
        // Hang up the child gracefully. A failed `try_wait` counts as alive:
        // signalling a dead child is harmless, not signalling a live one
        // blocked the reap forever.
        let observed = pty.child.try_wait();
        if needs_termination(&observed) {
            if let Err(err) = &observed {
                debug!(
                    ?err,
                    "pty child try_wait failed; assuming alive and terminating"
                );
            }
            Self::terminate_child_group(&mut pty).await;
        } else {
            trace!("pty child already exited");
        }
        // Close the writer channel so the writer thread exits.
        drop(self.pty_tx.take());
        // Reap without blocking this task; see `reap_bounded`.
        let child_pid = pty.child.process_id();
        if matches!(
            reap_bounded(|| pty.child.try_wait()).await,
            ReapOutcome::Expired
        ) {
            warn!("pty child did not exit within the reap budget; handing it to a reaper thread");
            spawn_detached_reaper(child_pid);
        }
        // Drop the PTY before joining: a reader blocked in `read(2)` (the
        // drain budget is only checked between reads) never returns while
        // we hold the master. Even then the reader's dup'd fd may keep it
        // blocked, so the joins are bounded.
        let reader_thread = pty.reader_thread.take();
        let writer_thread = pty.writer_thread.take();
        drop(pty);
        join_thread_bounded(reader_thread, "pty reader").await;
        join_thread_bounded(writer_thread, "pty writer").await;
    }

    /// Gracefully stop a live PTY child on teardown: `SIGHUP` both the
    /// foreground process group (snapshotted first, since the shell may exit
    /// at once) and the shell's group, wait up to
    /// [`super::PANE_KILL_GRACE`] for them to exit, then `SIGKILL` survivors.
    /// The master stays open so the foreground job can flush.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    pub(super) async fn terminate_child_group(pty: &mut PtyOwned) {
        let groups = pane_signal_groups(pty);
        if groups.is_empty() {
            hard_kill_child(pty);
            return;
        }
        if !hangup_pane_groups(&groups) {
            hard_kill_child(pty);
            return;
        }
        if await_pane_group_exit(pty, &groups).await {
            return;
        }
        hard_kill_pane_groups(&groups);
    }
}

/// Snapshot the process groups a hangup must reach, foreground job first
/// (`tcgetpgrp` fails once the shell exits).
fn pane_signal_groups(pty: &PtyOwned) -> Vec<nix::unistd::Pid> {
    use nix::unistd::Pid;

    let shell_group = pty
        .child
        .process_id()
        .and_then(|id| i32::try_from(id).ok())
        .map(Pid::from_raw);
    let foreground_group = pty
        .master
        .lock()
        .ok()
        .and_then(|master| master.process_group_leader())
        .filter(|id| *id > 0)
        .map(Pid::from_raw);

    let mut groups = Vec::with_capacity(2);
    if let Some(group) = foreground_group {
        groups.push(group);
    }
    if let Some(group) = shell_group
        && !groups.contains(&group)
    {
        groups.push(group);
    }
    groups
}

/// `SIGHUP` every group; `true` if at least one took it.
fn hangup_pane_groups(groups: &[nix::unistd::Pid]) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, killpg};

    let mut delivered = false;
    for &group in groups {
        match killpg(group, Signal::SIGHUP) {
            Ok(()) => delivered = true,
            Err(Errno::ESRCH) => {}
            Err(err) => debug!(?err, ?group, "SIGHUP to pane group failed"),
        }
    }
    delivered
}

/// Wait out [`super::pane_kill_grace`], `true` once every group exited.
/// Reaps the shell as it goes so its zombie does not keep its group alive.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
)]
async fn await_pane_group_exit(pty: &mut PtyOwned, groups: &[nix::unistd::Pid]) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::killpg;

    // Tests may start the ceiling only once a trap-started marker exists.
    #[cfg(test)]
    if let Some(gate) = super::pane_kill_grace_gate() {
        let hold = tokio::time::Instant::now() + super::PANE_KILL_GRACE_GATE_WAIT;
        loop {
            if let Err(err) = pty.child.try_wait() {
                debug!(?err, "try_wait during pane-kill grace gate failed");
            }
            if groups
                .iter()
                .all(|&group| matches!(killpg(group, None), Err(Errno::ESRCH)))
            {
                return true;
            }
            if gate.exists() || tokio::time::Instant::now() >= hold {
                break;
            }
            tokio::time::sleep(PANE_KILL_POLL).await;
        }
    }

    let deadline = tokio::time::Instant::now() + super::pane_kill_grace();
    while tokio::time::Instant::now() < deadline {
        if let Err(err) = pty.child.try_wait() {
            debug!(?err, "try_wait during pane-kill grace failed");
        }
        if groups
            .iter()
            .all(|&group| matches!(killpg(group, None), Err(Errno::ESRCH)))
        {
            return true;
        }
        tokio::time::sleep(PANE_KILL_POLL).await;
    }
    false
}

/// Whether a `try_wait` result needs termination: only a confirmed exit
/// says no; an error means "unknown", treated as alive.
const fn needs_termination(observed: &std::io::Result<Option<portable_pty::ExitStatus>>) -> bool {
    !matches!(observed, Ok(Some(_)))
}

/// What [`reap_bounded`] concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReapOutcome {
    /// The child was collected; nothing is left behind.
    Reaped,
    /// `try_wait` itself failed. Nothing more to try here.
    Failed,
    /// The budget ran out with the child still running.
    Expired,
}

/// Poll until the child is collected or [`PANE_KILL_REAP_BUDGET`] expires,
/// never blocking the task (a blocking `waitpid` on an escaped child would
/// freeze the shared runtime). Takes a closure so it is testable.
async fn reap_bounded<F>(mut poll: F) -> ReapOutcome
where
    F: FnMut() -> std::io::Result<Option<portable_pty::ExitStatus>>,
{
    let deadline = tokio::time::Instant::now() + PANE_KILL_REAP_BUDGET;
    loop {
        match poll() {
            Ok(Some(status)) => {
                debug!(?status, "pty child reaped");
                return ReapOutcome::Reaped;
            }
            Ok(None) => {}
            Err(err) => {
                debug!(?err, "pty child wait failed");
                return ReapOutcome::Failed;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return ReapOutcome::Expired;
        }
        tokio::time::sleep(PANE_KILL_POLL).await;
    }
}

/// Hand a child that outlived the reap budget to a detached `waitpid`
/// thread; nothing else would ever reap it.
fn spawn_detached_reaper(pid: Option<u32>) {
    let Some(pid) = pid.and_then(|raw| i32::try_from(raw).ok()) else {
        warn!("pty child outlived the reap budget and has no pid; it will stay a zombie");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("phux-pty-reaper".to_owned())
        .spawn(move || {
            let _ = nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(pid), None);
        });
    if let Err(err) = spawned {
        warn!(
            ?err,
            pid, "could not spawn a reaper thread; the child will stay a zombie"
        );
    }
}

/// Join a bridge thread, detaching it if it misses
/// [`PANE_KILL_REAP_BUDGET`] (threads cannot be cancelled; a bare `join`
/// could freeze the runtime).
async fn join_thread_bounded(handle: Option<std::thread::JoinHandle<()>>, what: &'static str) {
    let Some(handle) = handle else {
        return;
    };
    let deadline = tokio::time::Instant::now() + PANE_KILL_REAP_BUDGET;
    while !handle.is_finished() {
        if tokio::time::Instant::now() >= deadline {
            warn!(
                thread = what,
                "pty bridge thread did not exit within the budget; detaching it"
            );
            return;
        }
        tokio::time::sleep(PANE_KILL_POLL).await;
    }
    let _ = handle.join();
}

/// `SIGKILL` the child directly. `portable_pty`'s killer sends `SIGHUP`
/// and sleeps up to 200 ms on the actor task.
fn hard_kill_child(pty: &mut PtyOwned) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    if let Some(pid) = pty
        .child
        .process_id()
        .and_then(|raw| i32::try_from(raw).ok())
    {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        return;
    }
    // No pid to aim at: the library killer is all that is left.
    let _ = pty.child.kill();
}

/// Backstop: `SIGKILL` every surviving group.
fn hard_kill_pane_groups(groups: &[nix::unistd::Pid]) {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, killpg};

    for &group in groups {
        if !matches!(killpg(group, None), Err(Errno::ESRCH)) {
            let _ = killpg(group, Signal::SIGKILL);
        }
    }
}

#[cfg(test)]
mod teardown_policy_tests {
    use super::{ReapOutcome, join_thread_bounded, needs_termination, reap_bounded};
    use crate::terminal_actor::{PANE_KILL_POLL, PANE_KILL_REAP_BUDGET};

    /// A thread that never exits costs the budget and is then abandoned
    /// (the portable form of the slave-held-open hang).
    #[tokio::test(start_paused = true)]
    async fn a_thread_that_never_exits_is_detached_rather_than_joined() {
        let (release, parked) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            // Stand-in for a reader blocked on a slave nobody closes.
            let _ = parked.recv();
        });

        let started = tokio::time::Instant::now();
        join_thread_bounded(Some(handle), "never-exits").await;
        let waited = started.elapsed();

        assert!(
            waited >= PANE_KILL_REAP_BUDGET,
            "the join must serve its whole budget before detaching; gave up after {waited:?}",
        );
        // Let the thread finish so the test leaves nothing parked behind.
        let _ = release.send(());
    }

    /// A thread that exits is joined promptly.
    #[tokio::test(start_paused = true)]
    async fn a_thread_that_exits_is_joined_without_serving_the_budget() {
        let handle = std::thread::spawn(|| {});
        while !handle.is_finished() {
            std::thread::yield_now();
        }

        let started = tokio::time::Instant::now();
        join_thread_bounded(Some(handle), "exits").await;

        assert!(
            started.elapsed() < PANE_KILL_REAP_BUDGET,
            "a bridge thread that has already exited must not cost a budget",
        );
    }

    /// An unknown `try_wait` result means terminate.
    #[test]
    fn only_a_confirmed_exit_skips_termination() {
        assert!(
            !needs_termination(&Ok(Some(portable_pty::ExitStatus::with_exit_code(0)))),
            "a reaped child needs nothing"
        );
        assert!(needs_termination(&Ok(None)), "still running");
        assert!(
            needs_termination(&Err(std::io::Error::other("try_wait blew up"))),
            "an unreadable status must be read as alive: signalling a dead child is \
             harmless, stranding a live one is not",
        );
    }

    /// A child that never exits cannot hold the actor.
    #[tokio::test(start_paused = true)]
    async fn a_child_that_never_exits_expires_the_budget_instead_of_blocking() {
        let started = tokio::time::Instant::now();
        let mut polls = 0_u32;
        let outcome = reap_bounded(|| {
            polls += 1;
            Ok(None)
        })
        .await;

        assert_eq!(outcome, ReapOutcome::Expired);
        assert!(
            started.elapsed() >= PANE_KILL_REAP_BUDGET,
            "the reap must serve its whole budget before abandoning the child",
        );
        // It has to actually poll, not just sleep out the budget once.
        let expected = PANE_KILL_REAP_BUDGET.as_millis() / PANE_KILL_POLL.as_millis();
        assert!(
            u128::from(polls) >= expected,
            "expected ~{expected} polls across the budget, got {polls}",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_child_that_exits_is_reaped_without_serving_the_budget() {
        let started = tokio::time::Instant::now();
        let mut remaining = 2_u32;
        let outcome = reap_bounded(|| {
            if remaining == 0 {
                return Ok(Some(portable_pty::ExitStatus::with_exit_code(0)));
            }
            remaining -= 1;
            Ok(None)
        })
        .await;

        assert_eq!(outcome, ReapOutcome::Reaped);
        assert!(
            started.elapsed() < PANE_KILL_REAP_BUDGET,
            "a child that exits early must not cost the whole budget",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_try_wait_gives_up_rather_than_spinning_out_the_budget() {
        let outcome = reap_bounded(|| Err(std::io::Error::other("no such child"))).await;
        assert_eq!(
            outcome,
            ReapOutcome::Failed,
            "there is nothing to retry when the status itself is unreadable",
        );
    }
}
