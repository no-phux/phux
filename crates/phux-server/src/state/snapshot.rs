use std::collections::HashMap;

use phux_core::ids::SessionId;
use phux_core::session::Session;

use super::{AttachSnapshotPane, ServerState};

impl ServerState {
    /// Build a [`phux_protocol::wire::info::SessionSnapshot`] describing
    /// the entire registry plus the attaching client's initial focus.
    ///
    /// Used by the ATTACH handler in [`crate::runtime`] to populate the
    /// `ATTACHED` frame per SPEC §13. Allocates wire ids on demand so
    /// every entity in the registry gets one before this returns.
    ///
    /// `focus_session` is the resolved target of the ATTACH request;
    /// the attaching client's focused window/pane fall back to the
    /// session's `active` / window's `active` (tmux semantics).
    /// Returns `None` if `focus_session` has no active window or pane,
    /// since `SessionSnapshot::focused_window` / `focused_pane` are
    /// required fields on the wire.
    pub fn build_session_snapshot(
        &mut self,
        focus_session: SessionId,
    ) -> Option<phux_protocol::wire::info::SessionSnapshot> {
        use phux_protocol::wire::info::{SessionInfo, SessionSnapshot, TerminalInfo, WindowInfo};

        let attached_counts: HashMap<SessionId, u16> = {
            let mut counts: HashMap<SessionId, u16> = HashMap::new();
            for c in self.attached.values() {
                *counts.entry(c.session).or_insert(0) = counts
                    .get(&c.session)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
            }
            counts
        };

        let session_pairs: Vec<(SessionId, Session)> = self
            .registry
            .sessions()
            .map(|(id, s)| (id, s.clone()))
            .collect();

        let mut sessions = Vec::with_capacity(session_pairs.len());
        let mut windows = Vec::new();
        let mut panes = Vec::new();

        for (sid, session) in &session_pairs {
            let session_wire = self.idspace.intern_session(*sid);
            // Pre-intern the active window so `active_window` round-trips.
            let active_window_wire = session.active.map(|w| self.intern_window_wire(w));

            let created_at_unix_secs = session
                .created_at
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            sessions.push(
                SessionInfo::new(session_wire, session.name.clone())
                    .with_active_window(active_window_wire)
                    .with_created_at_unix_secs(created_at_unix_secs)
                    .with_window_count(u16::try_from(session.windows.len()).unwrap_or(u16::MAX))
                    .with_attached_client_count(attached_counts.get(sid).copied().unwrap_or(0)),
            );

            for (index, wid) in session.windows.iter().enumerate() {
                let Some(window) = self.registry.window(*wid).cloned() else {
                    continue;
                };
                let window_wire = self.intern_window_wire(*wid);
                let active_pane_wire = window.active.map(|p| self.intern_terminal_wire(p));

                // Layout-on-the-wire mirroring is its own concern;
                // for phux-byc.8 we ship `None` and let later tickets
                // translate `phux_core::LayoutNode` →
                // `phux_protocol::wire::info::LayoutNode`.
                windows.push(
                    WindowInfo::new(window_wire, session_wire, format!("window-{index}"))
                        .with_index(u16::try_from(index).unwrap_or(u16::MAX))
                        .with_active_pane(active_pane_wire),
                );

                for pid in &window.panes {
                    let Some(terminal) = self.registry.terminal(*pid).cloned() else {
                        continue;
                    };
                    let terminal_wire = self.intern_terminal_wire(*pid);
                    let cwd =
                        Some(terminal.cwd.to_string_lossy().into_owned()).filter(|s| !s.is_empty());
                    panes.push(
                        TerminalInfo::new(
                            terminal_wire,
                            window_wire,
                            terminal.dims.0,
                            terminal.dims.1,
                        )
                        .with_title(terminal.title.clone())
                        .with_cwd(cwd),
                    );
                }
            }
        }

        let session = self.registry.session(focus_session)?;
        let focused_window = session.active?;
        let focused_pane = self.registry.window(focused_window)?.active?;

        let focused_session_wire = self.idspace.intern_session(focus_session);
        let focused_window_wire = self.intern_window_wire(focused_window);
        let focused_pane_wire = self.intern_terminal_wire(focused_pane);

        Some(
            SessionSnapshot::new(focused_session_wire, focused_window_wire, focused_pane_wire)
                .with_sessions(sessions)
                .with_windows(windows)
                .with_panes(panes),
        )
    }

    /// Collect panes in `session` that have live actor handles, with wire ids.
    ///
    /// Protocol dispatch (`runtime::handle_attach`) uses this to drive per-pane
    /// snapshot/output setup without touching `Session`/`Window` internals.
    #[must_use]
    pub fn attach_snapshot_panes(&mut self, session: SessionId) -> Vec<AttachSnapshotPane> {
        let window_ids = self
            .registry
            .session(session)
            .map(|s| s.windows.clone())
            .unwrap_or_default();
        let mut panes = Vec::new();
        for wid in window_ids {
            let window_panes = self
                .registry
                .window(wid)
                .map(|w| w.panes.clone())
                .unwrap_or_default();
            for pid in window_panes {
                if let Some(handle) = self.terminal_handle(pid).cloned() {
                    panes.push(AttachSnapshotPane {
                        terminal_id: pid,
                        handle,
                        wire_terminal_id: self.intern_terminal_wire(pid),
                    });
                }
            }
        }
        panes
    }
}
