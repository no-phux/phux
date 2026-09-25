//! Resource geometry requests and non-resizing subscription intent.

use phux_protocol::wire::frame::TerminalRole;

use super::{ControlPlane, EngineEvent, EngineOutcome, FrameKind, ResourceId};

/// Local disposition of a targeted geometry request, never a server receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalResizeOutcome {
    /// Exactly one `RESIZE_TERMINAL` was queued. Read authoritative frames
    /// for the resulting geometry; this does not claim the server applied it.
    Queued,
    /// An axis was zero or exceeded the wire's `u16` range. Nothing was sent.
    InvalidSize,
    /// This client's configured attach role is observe-only. Nothing was sent.
    Observer,
    /// No live, confirmed subscription and current ready replica. Nothing was sent.
    NotReady,
}

impl ControlPlane {
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
        if self.geometry_observer() {
            return TerminalResizeOutcome::Observer;
        }
        if !self.geometry_ready(terminal_id) {
            return TerminalResizeOutcome::NotReady;
        }
        self.queue_frame(&FrameKind::ResizeTerminal {
            terminal_id: terminal_id.clone(),
            cols,
            rows,
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

    pub(super) fn geometry_observer(&self) -> bool {
        self.options.attach_role.unwrap_or_default().role == TerminalRole::Viewer
    }

    pub(super) fn follows_global_geometry(&self, terminal_id: &ResourceId) -> bool {
        !self.geometry_observer() && !self.preserve_terminal_geometry.contains(terminal_id)
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
