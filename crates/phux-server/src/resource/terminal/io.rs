//! Snapshot synthesis, input encoding, resize handling, and PTY
//! lifecycle plumbing for [`TerminalActor`].

use super::{
    Bytes, EncodedInputRequest, InputEncoderSnapshot, PANE_KILL_POLL, PANE_KILL_REAP_BUDGET,
    PaneOutput, PasteOutcome, PtyOwned, PtySize, ResyncAudience, ResyncReason, SizeReportSize,
    SnapshotBytes, TerminalActor, TerminalInput, WriteCompletion, debug, error, exit_outcome, mpsc,
    oneshot, trace, warn,
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
    ///
    /// `format` additionally renders the same capture through
    /// libghostty-vt's own Formatter into `ScreenState.rendered` (D9,
    /// fallback rung three): `0` (low 7 bits) none, `1` HTML, `2` VT.
    /// Bounded and side-effect-free like the rest of this read — one
    /// Formatter call over an explicit
    /// [`libghostty_vt::selection::Selection`], never a second extraction
    /// implementation (CONTRIBUTING).
    ///
    /// A render failure is non-fatal (review item 3): the plain
    /// projection still ships, `rendered` stays `None`, and
    /// `rendered_error` names why, logged at `warn`. The one exception is
    /// [`SynthesisError::RenderBudgetExceeded`], which this propagates as
    /// a hard `Err` — the caller (`reply_screen_state`) turns that into a
    /// typed `RESOURCE_EXHAUSTED` refusal instead of any reply, because a
    /// caller who asked for a rendering and would blow the byte budget
    /// needs to know to retry narrower, not receive nothing silently.
    ///
    /// When a rendering was requested, the reply omits the plain
    /// `lines`/`scrollback`/`soft_wrap`/`truncated` text (review item
    /// 2(c)): the capture already carries the same content, and shipping
    /// both would double an already-inflated (HTML/base64) reply for no
    /// reason.
    pub(super) fn screen_state(
        &self,
        pane: u32,
        scrollback: Option<u32>,
        cells: bool,
        format: u8,
    ) -> Result<phux_core::screen::ScreenState, crate::grid::SynthesisError> {
        let terminal = self.terminal.borrow();
        // Shared borrow: the read goes through a fresh per-call
        // `RenderState` (see the synthesizer body), so it never contends
        // with the tick path's `&mut` use of the pooled state.
        let synth = self.synth.borrow();
        let mut screen = synth.screen_state_with_scrollback(&terminal, pane, scrollback, cells)?;
        match synth.render_screen(&terminal, scrollback, format) {
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
    /// Was this pane's PTY silent long enough that the next output can be
    /// attributed to the input being handed off? A pane already streaming
    /// (a build, a `tail -f`) would pair the input with an unrelated chunk.
    fn pane_quiet_for_echo(&self) -> bool {
        self.last_output_at
            .get()
            .is_none_or(|at| at.elapsed() >= crate::perf::ECHO_QUIET_WINDOW)
    }

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
        let echo_probe = request.echo_probe;
        let len = request.bytes.len();
        let Some(tx) = self.pty_tx.as_ref() else {
            debug!("no PTY; encoded input discarded");
            if let Some(completion) = request.completion {
                completion.complete(WriteCompletion::NotWritten);
            }
            return;
        };
        match tx.try_send(request) {
            Ok(()) => {
                crate::perf::INPUT_EVENTS.incr();
                if echo_probe && self.pane_quiet_for_echo() {
                    self.last_input_at.set(Some(std::time::Instant::now()));
                }
                debug!(len, "input queued to PTY writer");
            }
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
        {
            self.reflow_tombstoned = self
                .invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);
        }
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
    /// client mirror via the ordinary `RESOURCE_OUTPUT` → `vt_write` path
    /// resets that mirror and repaints it from authoritative state. We
    /// reuse the existing output broadcast rather than the per-consumer
    /// state-sync path (`consumer_states`) because the runtime drives the
    /// broadcast/pump path; the q0e per-consumer tick is not wired into
    /// the runtime today.
    ///
    /// `audience` says which pumps replace their generation. It is still one
    /// broadcast item on the ordered channel even when addressed to a single
    /// pump: that is what orders the replacement cut after every live chunk
    /// the pump already saw, and what lets a fenced pump draining at memory
    /// speed reach it. Every other pump skips it.
    pub(super) fn broadcast_resync(&self, reason: ResyncReason, audience: ResyncAudience) {
        // No subscribers → nothing to resync. `receiver_count` is the
        // broadcast channel's live-subscriber count; the seed receiver
        // held by the actor was dropped at construction, so this is the
        // attached-pump count.
        if self.core.output_tx.receiver_count() == 0 {
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

    /// Best-effort reap the child if it has already exited. Called on
    /// PTY EOF — at that point the child has almost certainly exited
    /// (EOF on the master fd indicates the slave has been closed,
    /// which usually means the child has exited or detached). We try
    /// `try_wait` first to avoid blocking; if it returns `None` we
    /// leave the child alone (it might still be alive doing something
    /// odd; the shutdown path will deal with it).
    ///
    /// Returns the child's [`ExitOutcome`](phux_core::process::ExitOutcome):
    /// the code for a normal `_exit(n)`, the signal number for a death by
    /// signal (no longer flattened away), and neither when the child could
    /// not be reaped or the cause is unknown. `RESOURCE_CLOSED.exit_status`
    /// still carries only the code (SPEC §10.1 compact subset).
    pub(super) fn reap_child_if_any(&mut self) -> phux_core::process::ExitOutcome {
        use phux_core::process::ExitOutcome;
        let Some(pty) = self.pty.as_mut() else {
            return ExitOutcome::UNKNOWN;
        };
        // PTY EOF races the child becoming waitable: the master reads EOF
        // the moment the last slave fd closes, which can be a hair before
        // the kernel marks the process reapable. A single `try_wait` here
        // reported `exit_status: None` for children that exited cleanly
        // microseconds later, so RESOURCE_CLOSED lied to agents reading
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

    /// React to PTY EOF (the child went away): detach the PTY-read branch
    /// and record the exit facet. `exit_notify` is fired by the run loop
    /// *after* it flushes any pending gap resync, so a fenced consumer still
    /// gets the last screen before `RESOURCE_CLOSED` (phux-fpgl.28).
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
    /// The runtime decides whether this EOF closes the pane or replaces the
    /// child with a fresh default shell (last live Terminal in the session).
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
        // Land every in-flight native cut BEFORE the reset. A pending
        // bootstrap owns the canonical terminal (it is moved into the
        // snapshot capture for the duration), and this path is reached
        // exactly when a client may be attaching: `handle_pty_eof` keeps the
        // actor alive precisely so a `SnapshotRequest` racing the child's
        // exit still finds it. Resetting while the terminal is on loan used
        // to hit `NativeTerminalManager::reset`'s `unreachable!` and abort
        // the whole server process, taking every session on it with it.
        //
        // Invalidating is also the honest answer: the screen the cut
        // captured is about to be cleared, so any cursor derived from it is
        // stale. `install_replacement_pty` broadcasts an everyone-resync
        // right after, which is the resync the tombstoned pumps need, so the
        // returned targets need no separate address (phux-p5bo).
        self.invalidate_native_cuts_for_replacement();
        self.terminal.borrow_mut().reset_for_new_child();
    }

    /// Fail any in-flight native bootstrap and tombstone every outstanding
    /// cursor, returning the canonical terminal to the manager. No-op on a
    /// build without the native engine, which never loans the terminal out.
    fn invalidate_native_cuts_for_replacement(&mut self) {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            let _ = self.invalidate_all_native_cursors(
                phux_protocol::wire::frame::TombstoneReason::Other,
            );
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

    /// Let go of a retained pane's PTY once its process has exited
    /// (ADR-0124): close the master and the writer, so an exited pane holds
    /// no pseudoterminal and no descriptor while it waits to be purged. The
    /// grid and history live in the engine and stay.
    ///
    /// EOF reaped the child when it could. One it could not (it closed its
    /// tty but lives on, or `try_wait` failed) is handed to a reaper thread
    /// rather than left a zombie; the recorded exit decides, not a second
    /// `try_wait`, which could name a recycled pid.
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
        // The reader already saw EOF and the writer's channel just closed,
        // so both threads end on their own; dropping the handles detaches
        // them rather than blocking this actor on a join.
        drop(pty.reader_thread.take());
        drop(pty.writer_thread.take());
        drop(pty);
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
        // Close the PTY receiver FIRST, before anything below that waits.
        //
        // The reader-to-actor channel is bounded (`spawn::PTY_CHANNEL_DEPTH`),
        // and from here to the end of this function the actor never drains it
        // again — it is parked in the code below, not in its `select!` loop.
        // Leaving the receiver open therefore lets the queue fill and parks
        // the reader thread in `blocking_send`, which stops it calling
        // `read(2)` on the master. An unread PTY master accepts very little
        // before `write(2)` blocks (measured: 1024 bytes on macOS), so a
        // foreground job flushing a transcript wedges mid-flush — defeating
        // the grace window immediately below, whose entire purpose is to let
        // that flush finish.
        //
        // Dropping the receiver is what keeps the reader running rather than
        // stopping it: it switches to `spawn::drain_master_to_eof`, which
        // keeps reading and discarding so the far side of the PTY never
        // blocks. Discarding is not a behaviour change — nothing rendered
        // these bytes before either, since the actor is on its way out.
        self.pty_rx = None;
        // If the child is still alive, tear it down *gracefully* so a
        // foreground process (e.g. `claude`) gets a chance to flush before
        // we pull the rug — see `terminate_child_group`. If it already
        // exited this is a no-op; the reap below collects the zombie.
        //
        // A failed `try_wait` is treated as "still running", deliberately.
        // This branch used to log and fall through having sent no signal at
        // all: a child that was in fact alive was never asked to exit, never
        // killed, and the blocking reap then waited on it forever — freezing
        // every pane on this current-thread runtime (ADR-0003), not just this
        // one. Signalling a child that turns out to be dead is harmless
        // (`ESRCH`); failing to signal one that is alive is unbounded.
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
        // Drop the master so the reader thread sees EOF and exits.
        // We drop pty_tx so the writer thread sees a closed channel
        // and exits. Both happen automatically when `self.pty` /
        // `self.pty_tx` are dropped at the end of `run`, but doing it
        // here makes the thread joins below predictable.
        drop(self.pty_tx.take());
        // Reap the child so the OS releases its slot — without ever blocking
        // this task. See `reap_bounded`.
        let child_pid = pty.child.process_id();
        if matches!(
            reap_bounded(|| pty.child.try_wait()).await,
            ReapOutcome::Expired
        ) {
            warn!("pty child did not exit within the reap budget; handing it to a reaper thread");
            spawn_detached_reaper(child_pid);
        }
        // ORDER IS LOAD-BEARING: drop the PTY before joining the bridge
        // threads.
        //
        // Dropping `pty` closes this side's master handle and releases the
        // child, which is what lets the reader's `read(2)` return. Joining
        // first — as this did — deadlocks the whole server whenever the
        // reader is inside `spawn::drain_master_to_eof`, because that
        // function's budget is only observed BETWEEN reads: a `read(2)`
        // already blocked on a slave someone else still holds open never
        // returns to check it. Any process that escaped the snapshotted
        // groups is such a holder, and on a shared current-thread runtime
        // (ADR-0003) one of those freezes every pane on the server, forever.
        //
        // Dropping first is necessary but NOT sufficient — the reader holds
        // its own `dup`ed descriptor, so our close does not by itself end its
        // read — which is why the joins below are bounded rather than
        // unconditional.
        let reader_thread = pty.reader_thread.take();
        let writer_thread = pty.writer_thread.take();
        drop(pty);
        join_thread_bounded(reader_thread, "pty reader").await;
        join_thread_bounded(writer_thread, "pty writer").await;
    }

    /// Gracefully stop a still-running PTY child on pane teardown (phux-sw1).
    ///
    /// A pane close is a hangup: send `SIGHUP` to both the PTY's foreground
    /// process group and the session-leading shell group. Interactive shells
    /// put foreground jobs such as `claude` in a separate process group, so
    /// signaling only the shell group misses the process that needs to flush.
    /// Poll for both groups to exit within [`super::PANE_KILL_GRACE`], then `SIGKILL`
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

/// Snapshot the process groups a pane hangup must reach.
///
/// Signal the foreground job first. The shell may exit immediately on
/// SIGHUP, at which point tcgetpgrp can no longer recover this group.
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

/// Send `SIGHUP` to every snapshotted group, in order. `true` when at least
/// one group actually took the signal (an `ESRCH` group is already gone).
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

/// Wait out the [`super::pane_kill_grace`] budget, returning `true` once every
/// snapshotted group has exited.
///
/// Poll every snapshotted group, not just the shell child: the shell can
/// exit while a foreground job remains alive. Reap the shell as it exits
/// so its zombie does not keep the shell process group looking alive for
/// the entire grace period.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
)]
async fn await_pane_group_exit(pty: &mut PtyOwned, groups: &[nix::unistd::Pid]) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::killpg;

    // Tests may gate the ceiling on an observed trap-started marker so
    // a starved `/bin/sh` does not spend the flush budget waiting to
    // be scheduled (phux-ko7j). Production has no gate: the deadline
    // starts here, as it always has. Groups that already exited return
    // on the first poll in either case.
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

/// Does this `try_wait` result leave the child needing termination?
///
/// Only a confirmed exit says no. `Ok(None)` is "still running", and an `Err`
/// is "we do not know" — for which the safe reading is "alive". Signalling a
/// child that turns out to be dead is harmless (`ESRCH`); failing to signal a
/// live one used to strand it unsignalled and then block the reap on it
/// forever.
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

/// Poll `poll` until the child is collected or [`PANE_KILL_REAP_BUDGET`]
/// expires, without ever blocking the calling task.
///
/// `Child::wait` is a blocking `waitpid`. Calling it from the actor is only
/// sound if the child is guaranteed dead, and it is not: every path into this
/// function has *asked* the child to exit, but `hard_kill_pane_groups` only
/// reaches the process groups snapshotted before signalling. One that slips
/// through blocks `waitpid` forever — and because every pane actor shares one
/// current-thread runtime (ADR-0003), that is a frozen server, not a stuck
/// pane.
///
/// Takes a closure rather than the child so the policy can be tested without
/// a real PTY; see `reap_bounded_tests`.
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

/// Hand a child that outlived the reap budget to a detached thread that will
/// block in `waitpid` for as long as it takes.
///
/// Abandoning it outright — which is what this path used to do — leaks a
/// zombie for the lifetime of the server, and nothing else ever collects it:
/// phux installs no `SIGCHLD` handler and has no central reaper, and the
/// adopted-PTY child only reaps on an explicit poll. One thread parked in
/// `waitpid` is far cheaper than an entry in the process table that never
/// goes away, and unlike the actor task this thread is allowed to block.
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

/// Join a bridge thread, but give up and detach it if it does not exit inside
/// [`PANE_KILL_REAP_BUDGET`].
///
/// A bare `join()` here is a whole-server deadlock waiting to happen: the
/// reader can be blocked in `read(2)` on a slave that a process outside the
/// snapshotted groups still holds open, and the writer can be blocked writing
/// to a full one. A thread cannot be cancelled in Rust, so the bound is
/// "stop waiting", not "stop the thread" — we drop the handle and let it
/// finish on its own when its descriptor finally closes. That leaks at worst
/// one parked thread per abandoned pane, against freezing every pane on the
/// runtime.
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

/// `SIGKILL` the pane's child directly, bypassing `portable_pty`'s killer.
///
/// `ChildKiller::kill` is the wrong tool on two counts. It sends `SIGHUP`,
/// not `SIGKILL` — so the "fall back to killing the child" paths in
/// [`TerminalActor::terminate_child_group`] were only hanging it up again,
/// which is exactly what had already failed. And having sent it, the
/// implementation calls `std::thread::sleep` up to four times at 50 ms while
/// it polls, so a fallback that is supposed to be immediate blocked the actor
/// task — and with it every pane on the runtime — for up to 200 ms.
///
/// `nix` is already a dependency for the group signalling next door, so the
/// direct call costs nothing and says what it means.
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

/// Backstop: a group ignored the hangup (or is mid-flush past the
/// budget). Hard-kill every surviving snapshotted group.
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

    /// The portable gate on the join hang.
    ///
    /// The integration fixture for this (a detached process holding the slave
    /// open) cannot demonstrate the bug on a BSD-family kernel — see
    /// `pane_kill_is_bounded_when_a_detached_process_holds_the_slave_open` —
    /// so the property is pinned here instead, where it depends on nothing but
    /// our own code: a thread that will not exit must cost us the budget and
    /// then be abandoned, never an unbounded `join()`.
    #[tokio::test(start_paused = true)]
    async fn a_thread_that_never_exits_is_detached_rather_than_joined() {
        let (release, parked) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            // Parks until the test lets it go — the in-process stand-in for a
            // reader blocked in `read(2)` on a slave nobody will close.
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

    /// The other half: a thread that does exit must be joined promptly, not
    /// sat out for the full budget.
    #[tokio::test(start_paused = true)]
    async fn a_thread_that_exits_is_joined_without_serving_the_budget() {
        let handle = std::thread::spawn(|| {});
        // Let it finish before we start waiting, so this asserts the fast
        // path rather than racing it.
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

    /// The `Err` arm is the one that used to strand a live child unsignalled
    /// and then block the reap on it forever, so it is the one worth pinning:
    /// "we could not tell" must mean "terminate it", not "assume it is gone".
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

    /// The whole point of the bounded reap: a child that never exits must not
    /// hold the actor. Virtual time, so this asserts the policy rather than
    /// the wall clock.
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
