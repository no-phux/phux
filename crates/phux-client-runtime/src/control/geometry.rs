//! Resource geometry requests and non-resizing subscription intent.

use phux_protocol::wire::frame::TerminalRole;

use super::{
    ControlPlane, EngineEvent, EngineOutcome, FrameKind, ResourceId, Status, ViewportInfo,
};

/// Desired geometry and whether it still awaits the current attach barrier.
#[derive(Debug)]
pub(super) struct ViewportIntent {
    pub(super) desired: ViewportInfo,
    pub(super) pending: bool,
}

/// Local disposition of a targeted geometry request, never a server receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalResizeOutcome {
    /// Exactly one `RESIZE_TERMINAL` was queued. Read authoritative frames
    /// for the resulting geometry; this does not claim the server applied it.
    Queued,
    /// An axis was zero or exceeded the wire's `u16` range. Nothing was sent.
    InvalidSize,
    /// The terminal's configured attach role is observe-only. Nothing was sent.
    Observer,
    /// No live, confirmed subscription and current ready replica. Nothing was sent.
    NotReady,
}

impl ControlPlane {
    /// Submit desired viewport cells, clamping zero axes to one.
    ///
    /// The session viewport is a subscriber vote: the server applies its size
    /// policy across relevant subscribed terminals of the attached session,
    /// not just its active pane. Ordinary per-terminal subscriptions outside
    /// that session receive separate exact resizes. Preserving subscriptions
    /// and viewers skip that exact fanout; a session viewer still casts its
    /// viewport vote, as specified by L1.
    ///
    /// Desired geometry survives reconnect and changes during ATTACH are sent
    /// after `ATTACH_READY`. An explicit same-size submission reasserts intent;
    /// queuing is not application, and published grids remain authoritative.
    pub fn resize_viewport(&mut self, cols: u16, rows: u16) {
        self.resize_viewport_info(ViewportInfo::new(cols, rows));
    }

    /// Submit desired viewport cells and optional coherent pixel dimensions.
    /// Has the same lifecycle and authority semantics as [`Self::resize_viewport`].
    pub fn resize_viewport_info(&mut self, mut viewport: ViewportInfo) {
        viewport.cols = viewport.cols.max(1);
        viewport.rows = viewport.rows.max(1);
        self.viewport.desired = viewport;
        self.viewport.pending = true;
        self.publish_viewport();
    }

    pub(super) fn publish_viewport(&mut self) {
        if !self.viewport.pending
            || !self.handshake_ready
            || self.status.is_terminal()
            || (self.active_attach_id.is_some() && self.status != Status::Attached)
        {
            return;
        }
        self.viewport.pending = false;
        if self.attached_session.is_some() {
            self.queue_frame(&FrameKind::ViewportResize {
                viewport: self.viewport.desired,
            });
        }
        let foreign: Vec<_> = self
            .terminal_attached
            .iter()
            .filter(|id| self.follows_global_geometry(id))
            .cloned()
            .collect();
        for terminal_id in foreign {
            self.queue_frame(&FrameKind::ResizeTerminal {
                terminal_id,
                cols: self.viewport.desired.cols,
                rows: self.viewport.desired.rows,
                cell_px: None,
            });
        }
    }

    /// Request exact cell dimensions for one subscribed terminal.
    ///
    /// Does not change the global viewport, resize another resource, or update
    /// the local grid optimistically. Zero and out-of-wire-range dimensions
    /// are rejected rather than clamped. A viewer is refused locally.
    ///
    /// `Queued` is not an acknowledgement: the server checks authority, may
    /// drop stale requests, and sends no resize receipt. Published frames are
    /// authoritative readback. The server's explicit resize bypasses window-size
    /// selection; subsequent attach/detach/viewport operations can supersede it
    /// under view-derived policies (not under the manual policy).
    pub fn resize_terminal(
        &mut self,
        terminal_id: &ResourceId,
        cols: u32,
        rows: u32,
    ) -> TerminalResizeOutcome {
        let (Ok(cols), Ok(rows)) = (u16::try_from(cols), u16::try_from(rows)) else {
            return TerminalResizeOutcome::InvalidSize;
        };
        if cols == 0 || rows == 0 {
            return TerminalResizeOutcome::InvalidSize;
        }
        if self.geometry_observer(terminal_id) {
            return TerminalResizeOutcome::Observer;
        }
        if !self.geometry_ready(terminal_id) {
            return TerminalResizeOutcome::NotReady;
        }
        self.queue_frame(&FrameKind::ResizeTerminal {
            terminal_id: terminal_id.clone(),
            cols,
            rows,
            cell_px: None,
        });
        TerminalResizeOutcome::Queued
    }

    fn geometry_ready(&self, terminal_id: &ResourceId) -> bool {
        self.handshake_ready
            && !self.status.is_terminal()
            && self.terminal_is_admitted(terminal_id)
            && self.geometry_bootstrapped.contains(terminal_id)
            && self.attach_request_for_terminal(terminal_id).is_none()
            && self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.input_ready(terminal_id))
    }

    fn geometry_observer(&self, terminal_id: &ResourceId) -> bool {
        self.terminal_roles
            .get(terminal_id)
            .copied()
            .or(self.options.attach_role)
            .unwrap_or_default()
            .role
            == TerminalRole::Viewer
    }

    pub(super) fn follows_global_geometry(&self, terminal_id: &ResourceId) -> bool {
        !self.geometry_observer(terminal_id)
            && !self.preserve_terminal_geometry.contains(terminal_id)
    }

    /// Subscribe without requesting a resize or joining global viewport fanout.
    ///
    /// Returns the `TerminalAttached` correlation, or zero if already admitted
    /// or not negotiated. Calling this for an existing subscription is a no-op;
    /// it does not replace that subscription's original geometry policy.
    /// Automatic clients replay this subscription after reconnect. Manually
    /// driven clients reissue their attaches; the policy survives reconnect in
    /// either case, until detach/release/close. Stream recovery also preserves it.
    ///
    /// The role comes from `ControlOptions::attach_role`. This uses only
    /// `ATTACH_RESOURCE`, which carries no viewport; a separate session `ATTACH`
    /// still has its existing viewport semantics. Use resource subscriptions
    /// without a session attach for geometry-neutral observers and duplicates.
    pub fn attach_terminal_preserving_geometry(&mut self, terminal_id: &ResourceId) -> u32 {
        if !self.handshake_ready
            || self.status.is_terminal()
            || self.terminal_is_admitted(terminal_id)
        {
            return 0;
        }
        self.preserve_terminal_geometry.insert(terminal_id.clone());
        self.attach_terminal(terminal_id)
    }

    pub(super) fn replay_preserving_subscriptions(&mut self) {
        if !self.options.automatic_lifecycle {
            return;
        }
        let terminals: Vec<_> = self.preserve_terminal_geometry.iter().cloned().collect();
        for terminal_id in terminals {
            self.attach_terminal(&terminal_id);
        }
    }

    /// Remember only successfully applied bootstrap evidence from this socket.
    pub(super) fn note_geometry_bootstraps(
        &mut self,
        changes: Vec<(usize, ResourceId, bool)>,
        outcomes: &[EngineOutcome],
    ) {
        for (index, terminal_id, ready) in changes {
            if outcomes
                .get(index)
                .is_none_or(|outcome| outcome.error.is_some())
            {
                continue;
            }
            if ready {
                self.geometry_bootstrapped.insert(terminal_id);
            } else {
                self.geometry_bootstrapped.remove(&terminal_id);
            }
        }
    }
}

pub(super) fn bootstrap_changes(events: &[EngineEvent]) -> Vec<(usize, ResourceId, bool)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            EngineEvent::BootstrapReady { terminal_id, .. } => {
                Some((index, terminal_id.clone(), true))
            }
            EngineEvent::BootstrapBegin { terminal_id, .. }
            | EngineEvent::Closed { terminal_id, .. } => Some((index, terminal_id.clone(), false)),
            _ => None,
        })
        .collect()
}
