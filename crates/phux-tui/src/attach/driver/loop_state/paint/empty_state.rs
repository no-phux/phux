//! The view of an attached session with nothing arranged.

use super::super::{ChromeCtx, RepaintLevel, SidebarReservation, StatusBarPaint};

impl super::super::SessionLoop {
    /// An attached session with nothing arranged paints its empty state: the
    /// members still running with no view and the action that shows them
    /// again, else an empty persistent session's new-window hint. An
    /// ephemeral session with nothing running has already detached (R15).
    pub(in crate::attach::driver::loop_state) fn paint_empty_state<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        level: RepaintLevel,
    ) -> Option<StatusBarPaint> {
        if !self.mirror.workspace.groups.is_empty() || matches!(level, RepaintLevel::None) {
            return None;
        }
        let running = self.mirror.unarranged_members().len();
        let persistent =
            self.mirror.lifetime == phux_protocol::wire::frame::SessionLifetime::Persistent;
        if running == 0 && !persistent {
            return None;
        }
        let action = if running > 0 {
            "reopen-views"
        } else {
            "new-window"
        };
        let resolved = phux_config::keybind::ResolvedAction {
            action: action.to_owned(),
            args: std::collections::BTreeMap::new(),
        };
        let chord = crate::attach::action_registry::bound_chord_for(
            self.settings.keybindings.as_ref(),
            &resolved,
        );
        let lines = if running > 0 {
            crate::attach::paint::unarranged_lines(running, chord.as_deref())
        } else {
            crate::attach::paint::empty_session_lines(chord.as_deref())
        };
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
            &self.mirror.leaves,
        );
        Some(crate::attach::paint::paint_empty_session(
            out,
            &mut chrome,
            &lines,
            &mut self.mirror.panes,
        ))
    }
}
