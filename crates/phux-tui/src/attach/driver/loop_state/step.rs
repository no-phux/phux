//! One loop iteration: settle the view, then park on the wake-up sources.

use super::{
    AtomicBool, AttachError, Connection, ESC_FLUSH_IDLE, Ordering, RepaintLevel,
    SYNC_OUTPUT_WATCHDOG, SidebarReservation, Signal, StatusBarPainter, Step,
    desired_mouse_capture, exit_on_signal, mark_focused_seen, peer_gone, sleep_for_or_pending,
    sleep_until_or_pending, sync_hover_tracking, sync_mouse_capture, update_which_key_deadline,
};

impl super::SessionLoop {
    // ---- one loop iteration -------------------------------------------

    /// Settle the per-iteration view state, then park on every wake-up
    /// source until one of them fires.
    pub(in super::super) async fn step<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        needs_resync: Option<&AtomicBool>,
    ) -> Result<Step, AttachError> {
        let sidebar = self.sidebar();
        self.settle_iteration(out, sidebar, needs_resync)?;
        let stepped = self.select_next_event(conn, out, sidebar).await;
        // Whatever woke the loop may have completed a held switch's view.
        self.release_paint_hold_if_ready(out, self.sidebar());
        match stepped {
            // Any write into a server that already hung up (a SIGWINCH
            // resize racing a server crash, say) defers to the read side,
            // exactly as `send_unless_peer_gone` does: the next `recv`
            // drains what the server sent and then reports the EOF, which is
            // what the reconnect path and the explained endings key on.
            Err(err) if peer_gone(&err) && conn.write_hit_closed_peer() => {
                tracing::debug!(
                    ?err,
                    "write found the server gone; the read side names the ending"
                );
                Ok(Step::Continue)
            }
            stepped => stepped,
        }
    }

    /// Bring the outer terminal's modes, the attention ladder, and any
    /// dropped-backlog resync up to date before the loop parks.
    pub(super) fn settle_iteration<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        needs_resync: Option<&AtomicBool>,
    ) -> Result<(), AttachError> {
        // Capture follows focus. Closed panes are pruned so a
        // recycled ResourceId can never inherit a stale opt-out.
        if !self.mouse_optout.is_empty() {
            self.mouse_optout
                .retain(|id| self.mirror.panes.contains_key(id));
        }
        self.settle_focus_seen(out, sidebar);
        // Re-derive outer mouse tracking from the focused pane's opt-out every
        // iteration; a no-op when nothing changed.
        let want_capture = desired_mouse_capture(
            self.settings.mouse_capture,
            self.mirror.focused_resource.as_ref(),
            &self.mouse_optout,
        );
        sync_mouse_capture(out, want_capture).map_err(AttachError::Io)?;
        // Hover reporting follows the overlay stack (context menus).
        sync_hover_tracking(out, self.overlays.wants_pointer_hover()).map_err(AttachError::Io)?;
        self.repaint_after_resync(out, sidebar, needs_resync);
        crate::attach::render_prof::tick();
        Ok(())
    }

    /// Mark the focused pane seen (demoting its attention-ladder row) and
    /// repaint chrome in place when that changed a row. Checked every
    /// iteration so every way focus moves is covered.
    pub(super) fn settle_focus_seen<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if !mark_focused_seen(
            &mut self.mirror.panes,
            &mut self.review,
            self.mirror.focused_resource.as_ref(),
        ) {
            return;
        }
        if self.refresh_chrome() {
            self.repaint_view(out, sidebar, RepaintLevel::Chrome);
        }
    }

    /// The stdout writer dropped a stale backlog: repaint from scratch.
    pub(super) fn repaint_after_resync<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        needs_resync: Option<&AtomicBool>,
    ) {
        if !needs_resync.is_some_and(|flag| flag.swap(false, Ordering::AcqRel)) {
            return;
        }
        // The writer dropped bytes the renderers believe landed,
        // so no front buffer describes the screen any more.
        crate::attach::pane_state::invalidate_all_fronts(&mut self.mirror.panes);
        if self.overlays.is_active() {
            self.paint_overlay(out, sidebar);
        } else {
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
    }

    /// Arm this iteration's timers and park on every wake-up source. Stdin is
    /// polled before inbound frames; both arms are bounded, so neither starves.
    pub(super) async fn select_next_event<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<Step, AttachError> {
        self.arm_esc_flush();
        let flush_sleep = sleep_until_or_pending(self.esc_deadline);
        // (Dis)arm which-key from the resolver's current pending state.
        update_which_key_deadline(
            &mut self.which_key_deadline,
            self.settings
                .resolver
                .as_ref()
                .is_some_and(phux_config::keybind::Resolver::pending_at_prefix),
            self.settings.which_key.enabled,
            self.overlays.is_active(),
            tokio::time::Instant::now(),
            self.settings.which_key.delay,
        );
        let which_key_sleep = sleep_until_or_pending(self.which_key_deadline);
        // Status-bar repaint cadence; never fires for a bar with no polling widget.
        let status_tick = sleep_for_or_pending(
            self.settings
                .status_bar
                .as_ref()
                .and_then(StatusBarPainter::min_poll_interval),
        );
        // The pacer's settle deadline, armed only while a paint is owed, or
        // a held switch's deadline: the release after this step paints it.
        let paint_sleep = sleep_until_or_pending(
            [self.pacer.deadline(), self.paint_hold_deadline()]
                .into_iter()
                .flatten()
                .min(),
        );
        let sync_output_sleep = sleep_until_or_pending(
            self.mirror
                .panes
                .values()
                .filter_map(|slot| slot.sync_output_since)
                .map(|since| since + SYNC_OUTPUT_WATCHDOG)
                .min(),
        );
        // Down satellite panes probe the host inventory on a fixed cadence.
        self.arm_satellite_probe(tokio::time::Instant::now());
        let satellite_probe = sleep_until_or_pending(self.satellite_probe_at);
        let origin_fade = sleep_until_or_pending(self.next_input_origin_wake());
        // A coalesced agent notification comes due.
        let notify_due = sleep_until_or_pending(self.notifier.deadline());

        tokio::select! {
            biased;

            n = self.stdin.read(&mut self.stdin_buf) => self.on_stdin_timed(conn, out, sidebar, n).await,

            // Inbound frames, drained in a bounded batch so a burst paints once.
            frame = conn.recv() => {
                phux_perf::echo_hop::record("client_recv");
                let started = std::time::Instant::now();
                let result = self.on_server_frame(conn, out, sidebar, frame).await;
                phux_client::perf::FRAMES_WALL.record_elapsed(started);
                result
            },

            // The pacer window expired: settle every withheld pane in one frame
            // (a no-op while a switch holds; its release paints them all).
            () = paint_sleep => {
                self.settle_withheld_panes(out, sidebar);
                Ok(Step::Continue)
            }

            // An application that never sent `?2026l`: expose the mirror once.
            () = sync_output_sleep => {
                self.on_sync_output_timeout(out, sidebar);
                Ok(Step::Continue)
            }

            // Bare-ESC idle timeout.
            () = flush_sleep => self.on_esc_flush(conn, out, sidebar).await,

            // Which-key idle timeout. The pending prefix stays live, so the
            // next chord executes as if the popup never appeared.
            () = which_key_sleep => {
                self.on_which_key_timeout(out, sidebar);
                Ok(Step::Continue)
            }

            // SIGWINCH.
            _ = self.sigwinch.recv() => self.on_sigwinch(conn, out, sidebar).await,

            // Periodic status-bar repaint.
            () = status_tick => {
                self.on_status_tick(out, sidebar);
                Ok(Step::Continue)
            }

            // Ask which grey satellite panes can be reattached.
            () = satellite_probe => self.probe_satellites(conn).await,

            // Dim, then clear, a transient input-origin badge.
            () = origin_fade => {
                self.on_input_origin_phase(out, sidebar);
                Ok(Step::Continue)
            }

            // Post the agent notifications that waited out the coalesce window.
            () = notify_due => {
                self.on_notify_due(out);
                Ok(Step::Continue)
            }

            // Off-loop work finished: a background job (ADR-0172) toasts its
            // result and refreshes what it changed; a peer arrangement read
            // (ADR-0161) folds into the peer caches.
            done = next_off_loop(&self.jobs, self.peer_reader.as_mut()) => {
                self.on_off_loop(conn, out, sidebar, done).await
            }

            // ADR-0140: the hosts provider answered. Only a changed answer
            // repaints; the chrome drain is the same one a session-graph
            // change takes.
            Some(hosts) = self.hosts_rx.recv() => {
                self.on_hosts(out, sidebar, hosts);
                Ok(Step::Continue)
            }

            // SIGINT (130), SIGTERM (143), then SIGHUP (129). One arm, in
            // that order, so a biased select still prefers an earlier signal.
            code = recv_shutdown_signal(&mut self.sigint, &mut self.sigterm, &mut self.sighup) => {
                exit_on_signal(code)
            }
        }
    }

    /// Stdin readiness, with its echo-hop mark and input wall time.
    async fn on_stdin_timed<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        n: std::io::Result<usize>,
    ) -> Result<Step, AttachError> {
        if n.as_ref().is_ok_and(|read| *read > 0) {
            phux_perf::echo_hop::record("stdin");
        }
        let started = std::time::Instant::now();
        let result = self.on_stdin(conn, out, sidebar, n).await;
        phux_client::perf::INPUT_WALL.record_elapsed(started);
        result
    }

    /// Off-loop work finished: hand it to its own handler.
    async fn on_off_loop<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        done: OffLoop,
    ) -> Result<Step, AttachError> {
        match done {
            OffLoop::Job(done) => self.on_job_done(conn, out, sidebar, done).await,
            OffLoop::PeerReads(done) => self.on_peer_reads_done(conn, out, sidebar, done).await,
        }
    }

    /// SIGWINCH: resize the view, then keep looping.
    async fn on_sigwinch<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<Step, AttachError> {
        self.on_resize(conn, out, sidebar).await?;
        Ok(Step::Continue)
    }

    /// Ask which grey satellite panes can be reattached, then keep looping.
    async fn probe_satellites(&mut self, conn: &mut Connection) -> Result<Step, AttachError> {
        self.satellite_probe_at = None;
        self.request_host_inventory(conn).await?;
        Ok(Step::Continue)
    }

    /// Arm the bare-ESC timer only while a lone ESC is pending, anchored to
    /// the first iteration that saw it.
    fn arm_esc_flush(&mut self) {
        if self.parser.esc_pending() {
            self.esc_deadline
                .get_or_insert_with(|| tokio::time::Instant::now() + ESC_FLUSH_IDLE);
        } else {
            self.esc_deadline = None;
        }
    }
}

/// Work that finished off the attach loop.
enum OffLoop {
    /// A background job (ADR-0172).
    Job(crate::attach::jobs::JobDone),
    /// A peer arrangement read (ADR-0161).
    PeerReads(crate::attach::peer_arrangements::PeerReadDone),
}

/// The next finished job or peer arrangement read, jobs first.
async fn next_off_loop(
    jobs: &crate::attach::jobs::Jobs,
    peers: Option<&mut crate::attach::peer_arrangements::PeerReader>,
) -> OffLoop {
    tokio::select! {
        biased;
        done = jobs.next() => OffLoop::Job(done),
        done = crate::attach::peer_arrangements::next_done(peers) => OffLoop::PeerReads(done),
    }
}

/// The first of SIGINT, SIGTERM, or SIGHUP, as a shell-conventional code.
async fn recv_shutdown_signal(
    sigint: &mut Signal,
    sigterm: &mut Signal,
    sighup: &mut Signal,
) -> i32 {
    tokio::select! {
        biased;

        // Restore the terminal explicitly (Drop wouldn't fire on `exit(130)`).
        // `phux-roz`: this fires when the user hits Ctrl-C in the outer shell
        // after `phux attach` has entered the alt screen.
        _ = sigint.recv() => 130,

        // `kill <pid>` from a sibling tool, supervisor, or the user's
        // tmux/screen wrapping us.
        _ = sigterm.recv() => 143,

        _ = sighup.recv() => 129,
    }
}
