use super::{
    CanonicalTerminal, ProcessHandoff, PwdRequest, ScreenReply, ScreenRequest,
    SetDefaultColorsRequest, SnapshotBytes, SnapshotRequest, SynthesisError, TerminalActor,
    UpgradeHandleRequest, warn,
};

impl TerminalActor {
    /// Answer a bounded `SnapshotRequest` with replay bytes and their raw cut.
    pub(super) fn reply_bounded_snapshot(&self, req: SnapshotRequest) {
        let byte_limit = req
            .max_frames
            .checked_sub(2)
            .map(|chunks| chunks.saturating_mul(req.chunk_bytes).min(req.max_bytes))
            .ok_or(crate::grid::SynthesisError::LimitExceeded);
        let snap = byte_limit.and_then(|max_bytes| {
            self.synthesize_with_scrollback_bounded(req.scrollback, max_bytes, req.image_protocols)
        });
        if let Err(err) = &snap {
            warn!(error = %err, "bounded snapshot synthesis failed");
        }
        let _ = req
            .reply
            .send(snap.map(|snapshot| (snapshot, self.core.seq())));
    }

    /// Install a client's reported default palette, then acknowledge.
    pub(super) fn install_client_default_colors(&self, req: SetDefaultColorsRequest) {
        let result = match &mut *self.terminal.borrow_mut() {
            CanonicalTerminal::Plain(Some(terminal)) => {
                Self::install_default_colors(terminal, req.colors)
            }
            CanonicalTerminal::Plain(None) => Ok(()),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            CanonicalTerminal::Native(_) => {
                warn!("default palette update deferred while native cuts are active");
                Ok(())
            }
        };
        if let Err(err) = result {
            warn!(error = %err, "failed to install client default colors");
        }
        let _ = req.reply.send(());
    }

    /// Answer `GET_SCREEN`, falling back to an empty screen of the request's
    /// shape when projection fails.
    pub(super) fn reply_screen_state(&self, req: ScreenRequest) {
        let want_cells = req.cells;
        let reply = match self.screen_state(req.pane, req.scrollback, req.cells, req.format) {
            Ok(screen) => ScreenReply::Projection(Box::new(screen)),
            // Budget refusal must reach the caller typed, not as an empty screen.
            Err(SynthesisError::RenderBudgetExceeded { required, budget }) => {
                ScreenReply::TooLarge {
                    required_bytes: required,
                    budget_bytes: budget,
                }
            }
            Err(err) => {
                warn!(error = %err, "screen projection failed; replying with empty");
                ScreenReply::Projection(Box::new(phux_core::screen::ScreenState {
                    schema_version: phux_core::screen::SCHEMA_VERSION,
                    pane: req.pane,
                    cols: self.cols,
                    rows: self.rows,
                    cursor: None,
                    lines: Vec::new(),
                    scrollback: Vec::new(),
                    // Honour the requested shape: empty cells, not `None`.
                    cells: want_cells.then(Vec::new),
                    ..phux_core::screen::ScreenState::default()
                }))
            }
        };
        let _ = req.reply.send(reply);
    }

    /// ADR-0032: hand the upgrade producer this pane's PTY descriptors and
    /// a full replay snapshot.
    pub(super) fn reply_upgrade_handle(&self, req: UpgradeHandleRequest) {
        let mut snap = self
            .synthesize_with_scrollback(Some(0))
            .unwrap_or_else(|err| {
                warn!(error = %err, "upgrade snapshot synthesis failed; replying empty");
                SnapshotBytes {
                    cols: self.cols,
                    rows: self.rows,
                    bytes: Vec::new(),
                    scrollback: Vec::new(),
                }
            });
        let pty = self.pty.as_ref();
        let master_fd = pty.and_then(|p| {
            let master = p.master.lock().ok()?;
            let fd = master.as_raw_fd()?;
            // SAFETY: the master guard keeps `fd` open until `dup`
            // returns an independently owned descriptor.
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            let duplicate = rustix::io::dup(borrowed).ok();
            drop(master);
            duplicate
        });
        let child_pid = pty
            .and_then(|p| p.child.process_id())
            .and_then(|id| i32::try_from(id).ok());
        let cwd = pty
            .and_then(|p| p.child.process_id())
            .and_then(phux_resource::cwd_query::process_cwd)
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| {
                let last = self.last_known_cwd.borrow().clone();
                (!last.is_empty()).then_some(last)
            });
        let cell_px = (self.cell_px != (0, 0)).then_some(self.cell_px);
        snap.bytes.extend_from_slice(&self.program_status.replay());
        let _ = req.reply.send(ProcessHandoff {
            master_fd,
            child_pid,
            cols: self.cols,
            rows: self.rows,
            cell_px,
            osc_title: self.live_osc_title(),
            cwd,
            vt_replay_bytes: snap.bytes,
            scrollback_bytes: snap.scrollback,
        });
    }

    /// Answer with the PTY child's live cwd, or `None` (no PTY, no pid, or
    /// the query failed).
    pub(super) fn reply_pane_cwd(&self, req: PwdRequest) {
        let cwd = self
            .pty
            .as_ref()
            .and_then(|p| p.child.process_id())
            .and_then(phux_resource::cwd_query::process_cwd)
            .map(|p| p.to_string_lossy().into_owned());
        let _ = req.reply.send(cwd);
    }
}
