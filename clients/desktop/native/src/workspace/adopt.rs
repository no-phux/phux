//! Session terminals that no tab holds yet, offered tabs of their own.
use super::*;

impl Workspace {
    /// Give each home-session terminal that no tab here holds a tab of its
    /// own, so the tabs match the session's terminals without a click. The
    /// active tab stays put. A terminal is offered once: one moved out of
    /// this window or started in the background is not pulled back. A fresh
    /// window and one opened for a single terminal show only their own.
    pub(super) fn adopt_session_terminals(&mut self, connection: &Connection) {
        if self.options.fresh || self.options.initial_terminal.is_some() {
            return;
        }
        // Until a spawn answers, its terminal looks like anyone else's.
        if !self.spawns.is_empty() {
            return;
        }
        let unseen: Vec<ResourceId> = connection
            .panes()
            .iter()
            .filter(|pane| pane.session_name == self.session)
            .map(|pane| pane.terminal_id.clone())
            .filter(|terminal| self.considered.insert(terminal_label(terminal)))
            .collect();
        for terminal in unseen {
            if self.views_of(&terminal) > 0
                || self.awaiting.contains_key(&terminal_label(&terminal))
            {
                continue;
            }
            if connection.ready(&terminal) {
                self.land(
                    connection,
                    &terminal_label(&terminal),
                    Destination::QuietTab,
                );
                continue;
            }
            self.awaiting
                .insert(terminal_label(&terminal), Destination::QuietTab);
            let _ = connection
                .client
                .attach_terminal_preserving_geometry(&terminal);
        }
    }
}
