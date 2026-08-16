//! Snapshot synthesis, input encoding, resize handling, and PTY
//! lifecycle plumbing for [`TerminalActor`].

use super::{
    Bytes, EncodedInputRequest, InputEncoderSnapshot, PANE_KILL_GRACE, PANE_KILL_POLL, PaneOutput,
    PasteOutcome, PtyOwned, PtySize, ResyncReason, SizeReportSize, SnapshotBytes, TerminalActor,
    TerminalInput, WriteCompletion, debug, error, exit_status_to_wire, mpsc, trace, warn,
};

impl TerminalActor {
    /// Synthesize a snapshot of the current `Terminal` state. Exposed
    /// for tests that want to drive the synthesis path synchronously
    /// without going through the actor's `select!` loop.
    pub(super) fn synthesize(&self) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        self.synthesize_with_scrollback(None)
    }

    /// Synthesize an ATTACH snapshot, optionally priming the client's
    /// scrollback with retained history rows (`phux-9q5f`). `scrollback`
    /// follows the [`crate::grid::SnapshotSynthesizer::synthesize_with_scrollback`]
    /// convention. Exposed for tests that drive the synthesis path
    /// synchronously without the actor's `select!` loop.
    pub(super) fn synthesize_with_scrollback(
        &self,
        scrollback: Option<u32>,
    ) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        let terminal = self.terminal.borrow();
        // phux-uow0: the full snapshot uses a fresh RenderState internally, so
        // it needs only a shared borrow.
        let synth = self.synth.borrow();
        synth.synthesize_with_scrollback(&terminal, scrollback)
    }

    pub(super) fn synthesize_with_scrollback_bounded(
        &self,
        scrollback: Option<u32>,
        max_bytes: usize,
    ) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        let terminal = self.terminal.borrow();
        let synth = self.synth.borrow();
        synth.synthesize_with_scrollback_bounded(&terminal, scrollback, max_bytes)
    }

    /// Project the current `Terminal` grid into a structured
    /// [`phux_core::screen::ScreenState`], stamping `pane` as the
    /// wire-local id. Side-effect-free — the read path for `GET_SCREEN`.
    pub(super) fn screen_state(
        &self,
        pane: u32,
        scrollback: Option<u32>,
        cells: bool,
    ) -> Result<phux_core::screen::ScreenState, crate::grid::SynthesisError> {
        let terminal = self.terminal.borrow();
        // Shared borrow: the read goes through a fresh per-call
        // `RenderState` (see the synthesizer body), so it never contends
        // with the tick path's `&mut` use of the pooled state.
        let synth = self.synth.borrow();
        synth.screen_state_with_scrollback(&terminal, pane, scrollback, cells)
    }

    /// Publish the complete terminal-derived encoder state after a terminal
    /// mutation. Capture failures retain the previous good snapshot.
    pub(super) fn publish_input_snapshot(&self) {
        let terminal = self.terminal.borrow();
        match InputEncoderSnapshot::capture(&terminal, self.cell_px) {
            Ok(snapshot) => {
                self.input_snapshot_tx.send_replace(snapshot);
            }
            Err(err) => warn!(error = %err, "input encoder snapshot capture failed"),
        }
    }

    /// Translate a [`TerminalInput`] into PTY bytes via the per-pane
    /// encoders + the current terminal state.
    ///
    /// Returns `Ok(None)` when the event was deliberately dropped
    /// (e.g., focus events while DEC 1004 is off; rejected untrusted
    /// pastes). Returns `Err` on encoder failure; the caller logs and
    /// continues — a single bad input must not kill the actor.
    pub(super) fn encode_input(
        &self,
        input: &TerminalInput,
    ) -> Result<Option<Vec<u8>>, libghostty_vt::Error> {
        let terminal = self.terminal.borrow();
        match input {
            TerminalInput::Key(event) => {
                let mut enc = self.key_enc.borrow_mut();
                let bytes = enc.encode(event, &terminal)?;
                Ok(Some(bytes.to_vec()))
            }
            TerminalInput::Mouse(event) => {
                let mut enc = self.mouse_enc.borrow_mut();
                let bytes = enc.encode(event, &terminal, self.cell_px)?;
                Ok(Some(bytes.to_vec()))
            }
            TerminalInput::Focus(event) => {
                let mut enc = self.focus_enc.borrow_mut();
                let bytes = enc.encode(*event, &terminal)?;
                Ok(bytes.map(<[u8]>::to_vec))
            }
            TerminalInput::Paste(event) => {
                let mut enc = self.paste_enc.borrow_mut();
                match enc.encode(event, &terminal)? {
                    PasteOutcome::Encoded(bytes) => Ok(Some(bytes.to_vec())),
                    PasteOutcome::Rejected => Ok(None),
                }
            }
        }
    }

    /// Encode one input event and forward it to the PTY writer thread.
    /// Shared by the bounded `input_rx` drain in [`Self::run`]. A failed
    /// encode or a closed writer logs and is dropped — a single bad event
    /// must not kill the actor.
    pub(super) fn service_input(&self, input: &TerminalInput) {
        // Every arm below logs at debug or above: a dropped or empty input
        // is invisible to the caller (ROUTE_INPUT acks Ok regardless, per
        // SPEC §9 fire-and-forget), so this log is the only witness when a
        // key vanishes between the mailbox and the PTY.
        match self.encode_input(input) {
            Ok(Some(bytes)) => {
                if bytes.is_empty() {
                    debug!(?input, "input encoded to zero bytes; nothing to write");
                    return;
                }
                self.service_encoded_input(EncodedInputRequest::legacy(bytes));
            }
            Ok(None) => {
                debug!(?input, "input gated/dropped by encoder");
            }
            Err(err) => {
                warn!(error = %err, "input encode failed; dropping event");
            }
        }
    }

    /// Forward bytes encoded by the dedicated input lane to the PTY writer.
    ///
    /// Every branch that discards `request` here does so **before** it ever
    /// reaches a live writer thread — `write(2)` is provably never invoked —
    /// so an acknowledged request's completion is reported explicitly as
    /// [`WriteCompletion::NotWritten`] (phux-w7z2.60) rather than left to
    /// [`WriteCompletionSink`]'s pessimistic `Drop` fallback. A fire-and-forget
    /// request (`request.completion` is `None`) has nothing to report either
    /// way.
    pub(super) fn service_encoded_input(&self, request: EncodedInputRequest) {
        if request.bytes.is_empty() && request.completion.is_none() {
            return;
        }
        let len = request.bytes.len();
        let Some(tx) = self.pty_tx.as_ref() else {
            debug!("no PTY; encoded input discarded");
            if let Some(completion) = request.completion {
                completion.complete(WriteCompletion::NotWritten);
            }
            return;
        };
        match tx.try_send(request) {
            Ok(()) => debug!(len, "input queued to PTY writer"),
            // Dropping input is fire-and-forget per SPEC L1 §9, but it
            // is not a debug-level event: the bytes are gone, nothing
            // downstream reports it, and the caller is still acked `Ok`.
            // At `debug!` this was invisible by default, which is how a
            // payload split across several events could lose an
            // interior one and corrupt mid-stream with no trace
            // (phux-oxd7).
            Err(mpsc::error::TrySendError::Full(request)) => {
                warn!(len, "PTY writer queue full; dropping input");
                if let Some(completion) = request.completion {
                    completion.complete(WriteCompletion::NotWritten);
                }
            }
            // The writer thread is gone. Every subsequent byte for this
            // pane goes nowhere while output, snapshots, and acks all
            // keep working — the pane looks alive and is not.
            Err(mpsc::error::TrySendError::Closed(request)) => {
                error!(len, "PTY writer channel closed; pane input is dead");
                if let Some(completion) = request.completion {
                    completion.complete(WriteCompletion::NotWritten);
                }
            }
        }
    }

    /// Apply a resize to both the libghostty `Terminal` and the PTY
    /// kernel-side winsize. Idempotent; logs and continues on errors.
    ///
    /// Returns whether anything actually moved. A request that repeats the
    /// settled geometry changes no byte of grid, PTY winsize, or cell size,
    /// so the caller must not follow it with a resync broadcast either — a
    /// resync rotates the bootstrap generation, and rotating it for a resize
    /// that did not happen is exactly the wasted capture phux-a5xj is about.
    pub(super) fn handle_resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_px: Option<(u16, u16)>,
    ) -> bool {
        // libghostty has no concept of a zero-dimension grid: a 0-col or
        // 0-row resize fails with `InvalidValue` and leaves the grid at its
        // prior size. SPEC §10.5 already treats a zero-dimension viewport as
        // a no-op at the ATTACH path; clamp the live VIEWPORT_RESIZE path to
        // the same 1-cell minimum here so a `0x0` from a client (a host
        // terminal collapsing to nothing) can never reach libghostty.
        let cols = cols.max(1);
        let rows = rows.max(1);
        // Repeating the settled geometry is a true no-op. In particular, a
        // second same-size subscriber must not invalidate every independent
        // native history cursor merely because viewport arbitration emitted
        // the current winner again.
        if cols == self.cols && rows == self.rows && cell_px.is_none_or(|cell| cell == self.cell_px)
        {
            return false;
        }
        // Sticky cell size: see the `ResizeRequest::cell_px` doc.
        if let Some(cell) = cell_px {
            self.cell_px = cell;
        }
        let (cell_w, cell_h) = self.cell_px;

        // `Terminal::resize` takes the per-cell pixel size and derives the
        // terminal's pixel dimensions (`cells x cell size`) for XTWINOPS
        // size reports, mode-2048 in-band notifications, and image
        // protocols. Seeded to `DEFAULT_CELL_PX` and replaced by a client's
        // reported cell size, so it is always nonzero — pixel probes inside
        // the pane never see a zero text area.
        //
        // A both-axes shrink in a single resize() call once overflowed
        // libghostty's `PageList.resizeCols` (phux-y06, the SIGABRT
        // reproduced by the resize-extremes storm); libghostty-vt 0.2.0
        // covers that path, so a both-shrink is a single safe call.
        let applied = {
            let mut term = self.terminal.borrow_mut();
            let result = term.resize(cols, rows, u32::from(cell_w), u32::from(cell_h));
            if let Err(err) = result {
                warn!(?err, cols, rows, "terminal resize failed");
            }
            // Cache the dims libghostty actually settled on, never the
            // requested dims: on error (e.g. a clamped 0 that still failed)
            // the grid is unchanged, so caching the request would desync
            // the cache from the real grid size.
            (term.cols().unwrap_or(cols), term.rows().unwrap_or(rows))
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);
        self.cols = applied.0;
        self.rows = applied.1;
        self.size_report.set(SizeReportSize {
            rows: applied.1,
            columns: applied.0,
            cell_width: u32::from(cell_w),
            cell_height: u32::from(cell_h),
        });
        self.publish_input_snapshot();
        // A resize reflows the grid: every consumer reference is rebuilt
        // on the next diff, so force the next tick to walk (phux-4l0), and
        // force the next detector tick to re-scan (ADR-0046) — a reflow can
        // move the prompt box, which is a region the rules depend on.
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
        if let Some(pty) = &self.pty {
            // The kernel `winsize` pixel fields are the whole text area;
            // saturate rather than wrap if an enormous grid on a dense
            // display overflows the u16 (the kernel field is no wider).
            let size = PtySize {
                rows: applied.1,
                cols: applied.0,
                pixel_width: applied.0.saturating_mul(cell_w),
                pixel_height: applied.1.saturating_mul(cell_h),
            };
            if let Ok(master) = pty.master.lock()
                && let Err(err) = master.resize(size)
            {
                warn!(
                    ?err,
                    cols = applied.0,
                    rows = applied.1,
                    "pty resize ioctl failed"
                );
            }
        }
        true
    }

    /// Broadcast a full synthesized snapshot of the canonical `Terminal`'s
    /// current grid to every attached client, as an in-band
    /// [`PaneOutput::Resync`].
    ///
    /// Two callers: a resize that reflowed the grid (phux-8v1, below), and a
    /// `resync_only` request from an output pump that dropped bytes past the
    /// broadcast buffer (`RecvError::Lagged`) and needs its consumer's mirror
    /// rebuilt. Both want the same thing — the authoritative grid re-sent on the
    /// ordered output channel so it cleanly supersedes whatever the client last
    /// applied, with no double-apply or lost output.
    ///
    /// Why this is needed: a resize triggers an *independent* reflow on
    /// both the server's canonical `Terminal` and each client's mirror
    /// `Terminal`. Those reflows can diverge — libghostty's cols-shrink
    /// reflow does not reproduce the client mirror's content identically,
    /// dropping rows — so after a resize the client mirror and the server
    /// grid disagree. The live output path (the PTY-byte broadcast fanned
    /// out by the per-attach pump in `runtime.rs`) only carries *new* PTY
    /// bytes, so the historical grid content is never re-sent and the
    /// divergence is permanent: the user sees lost / duplicated rows
    /// ("repeating/duplicated characters on resize").
    ///
    /// The synthesized bytes from [`SnapshotSynthesizer::synthesize`] open
    /// with a `DECSTR + ED2 + home` reset preamble, so feeding them to the
    /// client mirror via the ordinary `TERMINAL_OUTPUT` → `vt_write` path
    /// resets that mirror and repaints it from authoritative state. We
    /// reuse the existing output broadcast rather than the per-consumer
    /// state-sync path (`consumer_states`) because the runtime drives the
    /// broadcast/pump path; the q0e per-consumer tick is not wired into
    /// the runtime today.
    pub(super) fn broadcast_resync(&self, reason: ResyncReason) {
        // No subscribers → nothing to resync. `receiver_count` is the
        // broadcast channel's live-subscriber count; the seed receiver
        // held by the actor was dropped at construction, so this is the
        // attached-pump count.
        if self.output_tx.receiver_count() == 0 {
            return;
        }
        match self.synthesize() {
            Ok(snap) => {
                debug!(
                    bytes = snap.bytes.len(),
                    "resize resync: snapshot broadcast"
                );
                // A `Lagged`/no-receiver send error is benign here — the
                // next PTY output or a re-attach snapshot re-syncs.
                // phux-3ns5: ship the post-reflow grid as a `Resync` (→
                // `TERMINAL_SNAPSHOT`) carrying the settled dims, so the
                // client mirror resizes to `(cols, rows)` before applying
                // the replay. Delivered as raw output it could not resize
                // the mirror, stranding a resize-grow with blank space.
                let _ = self.output_tx.send(PaneOutput::Resync {
                    cols: self.cols,
                    rows: self.rows,
                    reason,
                    base_seq: self.raw_seq,
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

    /// Best-effort reap the child if it has already exited. Called on
    /// PTY EOF — at that point the child has almost certainly exited
    /// (EOF on the master fd indicates the slave has been closed,
    /// which usually means the child has exited or detached). We try
    /// `try_wait` first to avoid blocking; if it returns `None` we
    /// leave the child alone (it might still be alive doing something
    /// odd; the shutdown path will deal with it).
    ///
    /// Returns the exit status in the shape the `TERMINAL_CLOSED` wire
    /// frame wants (phux-4li.11): `Some(code)` for a normal `_exit(n)`,
    /// `None` for signal-killed children or otherwise-unknown exits.
    /// `portable_pty::ExitStatus.signal` is the discriminator — a
    /// non-`None` signal name means the kernel reports the death as
    /// signal-driven, which collapses to `exit_status = None` on the
    /// wire per the SPEC §10.1 compact-subset rule.
    pub(super) fn reap_child_if_any(&mut self) -> Option<i32> {
        let pty = self.pty.as_mut()?;
        // PTY EOF races the child becoming waitable: the master reads EOF
        // the moment the last slave fd closes, which can be a hair before
        // the kernel marks the process reapable. A single `try_wait` here
        // reported `exit_status: None` for children that exited cleanly
        // microseconds later, so TERMINAL_CLOSED lied to agents reading
        // exit codes. Retry briefly. The blocking sleep is deliberate:
        // this runs on the single current-thread runtime, but only once
        // per pane lifetime, and the budget is small; an async retry would
        // need to move `child` out of `self.pty`, which the shutdown path
        // still owns. A child that closed its slave but keeps running
        // (a daemonizer) exhausts the budget and reports `None`, as before.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        loop {
            match pty.child.try_wait() {
                Ok(Some(status)) => {
                    debug!(?status, "child reaped on PTY EOF");
                    return exit_status_to_wire(&status);
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(None) => {
                    trace!("PTY EOF but child still alive — leaving to shutdown path");
                    return None;
                }
                Err(err) => {
                    debug!(?err, "child try_wait failed on PTY EOF");
                    return None;
                }
            }
        }
    }

    /// React to PTY EOF (the child went away): detach the PTY-read branch
    /// and notify the runtime so it can broadcast `TERMINAL_CLOSED`.
    ///
    /// Dropping `pty_rx` parks the pump's `select!` arm forever, but the
    /// actor deliberately stays alive — it must remain reachable for
    /// late-arriving `SnapshotRequest`s (a client attaching just after the
    /// child exited) and for orderly shutdown via the cancellation token.
    /// The child is reaped here so we don't leave a zombie waiting for the
    /// explicit shutdown signal. (phux-it8: firing `exit_notify` is what
    /// lets attached clients learn the shell exited instead of freezing in
    /// alt-screen.)
    ///
    /// TODO(phux-9gw): multi-pane lifecycle — when a session has more than
    /// one pane, a single EOF should switch focus to a sibling rather than
    /// detach the whole session. Today sessions are 1:1 with panes in
    /// practice so the simpler "EOF → detach attached" model is correct.
    pub(super) fn handle_pty_eof(&mut self) {
        debug!("PTY EOF; firing exit_notify and keeping actor alive for late snapshot/input drain");
        self.pty_rx = None;
        let exit_status = self.reap_child_if_any();
        if let Some(tx) = self.exit_notify.take() {
            let _ = tx.send(exit_status);
        }
    }

    /// Tear down the PTY: gracefully stop the child if still alive, drop
    /// the master (which sends EOF to the slave and unblocks the reader
    /// thread), and join the bridge threads. Best-effort: errors are
    /// logged, not propagated, because we're on the shutdown path.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    pub(super) async fn shutdown_pty(&mut self) {
        let Some(mut pty) = self.pty.take() else {
            return;
        };
        // If the child is still alive, tear it down *gracefully* so a
        // foreground process (e.g. `claude`) gets a chance to flush before
        // we pull the rug — see `terminate_child_group`. If it already
        // exited this is a no-op; `wait` below reaps the zombie.
        match pty.child.try_wait() {
            Ok(Some(_status)) => {
                trace!("pty child already exited");
            }
            Ok(None) => {
                Self::terminate_child_group(&mut pty).await;
            }
            Err(err) => {
                debug!(?err, "pty child try_wait failed");
            }
        }
        // Drop the master so the reader thread sees EOF and exits.
        // We drop pty_tx so the writer thread sees a closed channel
        // and exits. Both happen automatically when `self.pty` /
        // `self.pty_tx` are dropped at the end of `run`, but doing it
        // here makes the thread joins below predictable.
        drop(self.pty_tx.take());
        // Reap the child so the OS releases its slot.
        match pty.child.wait() {
            Ok(status) => debug!(?status, "pty child reaped"),
            Err(err) => debug!(?err, "pty child wait failed"),
        }
        // We can't drop `pty.master` separately because it's behind an
        // Arc<Mutex<_>> — the Arc strong count drops when `pty` falls
        // out of scope at the end of this function.
        if let Some(handle) = pty.reader_thread.take() {
            // Bounded wait: if the reader hasn't exited inside a small
            // budget we move on. Joining unconditionally would hang
            // the shutdown path if the reader is wedged in a blocking
            // read on a pty fd that won't EOF (rare but possible).
            //
            // In practice EOF arrives immediately after `master` drops
            // at the end of this function; we accept the join-on-drop
            // ordering risk because the reader thread is owned and
            // bounded by us. The unwrap below is safe because the
            // thread itself can't panic — it's a tight loop on Read.
            let _ = handle.join();
        }
        if let Some(handle) = pty.writer_thread.take() {
            let _ = handle.join();
        }
        drop(pty);
    }

    /// Gracefully stop a still-running PTY child on pane teardown (phux-sw1).
    ///
    /// A pane close is a hangup: send `SIGHUP` to both the PTY's foreground
    /// process group and the session-leading shell group. Interactive shells
    /// put foreground jobs such as `claude` in a separate process group, so
    /// signaling only the shell group misses the process that needs to flush.
    /// Poll for both groups to exit within [`PANE_KILL_GRACE`], then `SIGKILL`
    /// any survivors as a backstop. The PTY master stays open for the duration,
    /// so the foreground process can still write during the grace window.
    ///
    /// This replaces an immediate `std::process::Child::kill` (a `SIGKILL` of
    /// the shell pid alone, with no grace), which killed a foreground agent
    /// before it could persist its transcript. The foreground group is
    /// snapshotted from the PTY before signaling to avoid losing it when the
    /// shell exits. Falls back to the library kill if no group can be found.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    pub(super) async fn terminate_child_group(pty: &mut PtyOwned) {
        use nix::errno::Errno;
        use nix::sys::signal::{Signal, killpg};
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

        // Signal the foreground job first. The shell may exit immediately on
        // SIGHUP, at which point tcgetpgrp can no longer recover this group.
        let mut groups = Vec::with_capacity(2);
        if let Some(group) = foreground_group {
            groups.push(group);
        }
        if let Some(group) = shell_group
            && !groups.contains(&group)
        {
            groups.push(group);
        }
        if groups.is_empty() {
            let _ = pty.child.kill();
            return;
        }

        let mut delivered = false;
        for &group in &groups {
            match killpg(group, Signal::SIGHUP) {
                Ok(()) => delivered = true,
                Err(Errno::ESRCH) => {}
                Err(err) => debug!(?err, ?group, "SIGHUP to pane group failed"),
            }
        }
        if !delivered {
            let _ = pty.child.kill();
            return;
        }

        // Poll every snapshotted group, not just the shell child: the shell can
        // exit while a foreground job remains alive. Reap the shell as it exits
        // so its zombie does not keep the shell process group looking alive for
        // the entire grace period.
        let deadline = tokio::time::Instant::now() + PANE_KILL_GRACE;
        while tokio::time::Instant::now() < deadline {
            if let Err(err) = pty.child.try_wait() {
                debug!(?err, "try_wait during pane-kill grace failed");
            }
            if groups
                .iter()
                .all(|&group| matches!(killpg(group, None), Err(Errno::ESRCH)))
            {
                return;
            }
            tokio::time::sleep(PANE_KILL_POLL).await;
        }

        // Backstop: a group ignored the hangup (or is mid-flush past the
        // budget). Hard-kill every surviving snapshotted group.
        for group in groups {
            if !matches!(killpg(group, None), Err(Errno::ESRCH)) {
                let _ = killpg(group, Signal::SIGKILL);
            }
        }
    }
}
