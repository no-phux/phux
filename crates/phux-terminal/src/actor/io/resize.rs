use super::{
    Bytes, PtySize, ResourceOutput, ResyncAudience, ResyncReason, SizeReportSize, TerminalActor,
    debug, warn,
};

impl TerminalActor {
    /// Apply a resize to the `Terminal` and the PTY winsize, returning
    /// whether the grid reflowed. An unchanged grid earns no resync (that
    /// would rotate the bootstrap generation for nothing). A cell-pixel
    /// change at the same grid updates canonical pixel geometry, winsize and
    /// input encoding without retiring history or replacing a scrolled view.
    pub(in crate::actor) fn handle_resize(
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
        // native history cursors. A failed ioctl stays pending and retries.
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
        if applied == (self.cols, self.rows) {
            self.install_cell_px(cell_w, cell_h);
            return false;
        }
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            self.reflow_tombstoned = self
                .invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);
        }
        self.cols = applied.0;
        self.rows = applied.1;
        self.install_cell_px(cell_w, cell_h);
        // A reflow rebuilds every consumer reference and may move detector
        // regions: force both ticks to rescan.
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
        true
    }

    /// Record a cell size for winsize, XTWINOPS, and mouse encoding, and
    /// push it to the kernel. Does not touch the grid or native cursors.
    fn install_cell_px(&mut self, cell_w: u16, cell_h: u16) {
        self.cell_px = (cell_w, cell_h);
        self.size_report.set(SizeReportSize {
            rows: self.rows,
            columns: self.cols,
            cell_width: u32::from(cell_w),
            cell_height: u32::from(cell_h),
        });
        self.publish_input_snapshot();
        self.pty_resize_pending = true;
        self.apply_pty_resize();
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

    /// Broadcast a text-only synthesized snapshot as an in-band
    /// [`ResourceOutput::Resync`]; image-capable pumps capture their own image replay.
    ///
    /// Used after a reflow (server and client reflow independently and can
    /// diverge, and live output never re-sends history) and for pumps that
    /// lagged past the broadcast buffer. The replay opens with a reset, so it
    /// cleanly supersedes the client mirror. `audience` picks which pumps
    /// replace their generation; it is still one ordered broadcast item, so
    /// it lands after every chunk those pumps already saw.
    pub(in crate::actor) fn broadcast_resync(
        &self,
        reason: ResyncReason,
        audience: ResyncAudience,
    ) {
        // No attached pumps (the actor's seed receiver was dropped).
        if self.core.output_tx.receiver_count() == 0 {
            return;
        }
        // A replacement creates a fresh consumer replica. Replay history at the
        // same cut too, or the first phone/keyboard resize erases scrollback.
        // Leave half the shared 64 MiB staging budget for capability rewriting.
        match self.synthesize_with_scrollback_bounded(
            Some(super::super::DEFAULT_REPLAY_SCROLLBACK_LINES),
            32 * 1024 * 1024,
            phux_protocol::caps::ImageProtocolSet::new(),
        ) {
            Ok(snap) => {
                let replay = if snap.scrollback.is_empty() {
                    Bytes::from(snap.bytes)
                } else {
                    let mut replay = snap.scrollback;
                    replay.reserve_exact(snap.bytes.len());
                    replay.extend_from_slice(&snap.bytes);
                    Bytes::from(replay)
                };
                debug!(bytes = replay.len(), "resize resync: snapshot broadcast");
                // Send errors are benign. `Resync` carries the settled dims
                // so the mirror resizes before applying the replay.
                let _ = self.core.output_tx.send(ResourceOutput::Resync {
                    cols: self.cols,
                    rows: self.rows,
                    reason,
                    audience,
                    base_seq: self.core.seq(),
                    bytes: replay,
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
}
