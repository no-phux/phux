//! Tab, placement, focus and user-close operations.
use super::*;

impl Workspace {
    /// Select a tab that exists.
    pub fn select_tab(&mut self, id: &str) {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.active_id = id.to_owned();
        }
    }

    /// Move the active tab by `step`, wrapping.
    pub fn step_tab(&mut self, step: i8) {
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == self.active_id)
            .unwrap_or(0);
        if let Some(next) = step_index(index, step, self.tabs.len()) {
            self.active_id = self.tabs[next].id.clone();
        }
    }

    /// Select by index. A negative index selects the last tab.
    pub fn select_index(&mut self, index: isize) {
        let tab = if index < 0 {
            self.tabs.last()
        } else {
            self.tabs.get(index as usize)
        };
        if let Some(tab) = tab {
            self.active_id = tab.id.clone();
        }
    }

    /// Reorder tabs.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        reorder(&mut self.tabs, from, to);
    }

    /// Set or clear a tab title. Blank clears it. Titles are at most 80 characters.
    pub fn rename_tab(&mut self, id: &str, title: &str) {
        let title = title.trim();
        let title = (!title.is_empty()).then(|| title.chars().take(80).collect());
        if let Some(tab) = self.tab_mut(id) {
            tab.title = title;
            refocus(tab);
        }
    }

    /// Release the tab's views. Does not terminate processes.
    pub fn close_tab(&mut self, connection: &Connection, id: &str) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let tab = self.tabs.remove(index);
        self.release(connection, placements(&tab.root));
        if self.active_id == id {
            self.active_id = self
                .tabs
                .get(index.min(self.tabs.len().saturating_sub(1)))
                .map(|tab| tab.id.clone())
                .unwrap_or_default();
        }
    }

    /// Focus a placement. A zoom follows the focus.
    pub fn focus_placement(&mut self, tab_id: &str, placement_id: &str) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let Some(placement) = find_placement(&tab.root, placement_id).cloned() else {
            return;
        };
        let unchanged = tab.focused_id == placement_id && self.active_id == tab_id;
        self.claim(&placement);
        if unchanged {
            return;
        }
        if let Some(tab) = self.tab_mut(tab_id) {
            tab.focused_id = placement_id.to_owned();
            if tab.zoomed_id.is_some() {
                tab.zoomed_id = Some(placement_id.to_owned());
            }
        }
        self.active_id = tab_id.to_owned();
    }

    pub(super) fn focus_from_active(
        &mut self,
        target: impl FnOnce(&LayoutNode, &str) -> Option<String>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if let Some(target) = target(&tab.root, &tab.focused_id) {
            let tab_id = tab.id.clone();
            self.focus_placement(&tab_id, &target);
        }
    }

    /// Focus the neighbour in `direction`.
    pub fn focus_direction(&mut self, direction: Direction) {
        self.focus_from_active(|root, from| neighbor(root, from, direction));
    }

    /// Cycle panes, wrapping.
    pub fn cycle_pane(&mut self, step: i8) {
        self.focus_from_active(|root, from| cycle(root, from, step));
    }

    /// Zoom the focused pane, or clear zoom. A single pane does not zoom.
    pub fn toggle_zoom(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if placements(&tab.root).len() > 1 && tab.zoomed_id.take().is_none() {
            tab.zoomed_id = Some(tab.focused_id.clone());
        }
    }

    /// Nudge the nearest divider by Ghostty's step.
    pub fn nudge(&mut self, direction: Direction) {
        if let Some(tab) = self.active_tab_mut() {
            nudge(&mut tab.root, &tab.focused_id, direction, 0.04);
            refocus(tab);
        }
    }

    /// Equalize the active tab.
    pub fn equalize_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            equalize(&mut tab.root);
        }
    }

    /// Set one split ratio in the active tab.
    pub fn resize_split(&mut self, split_id: &str, ratio: f32) {
        if let Some(tab) = self.active_tab_mut() {
            set_ratio(&mut tab.root, split_id, ratio);
        }
    }

    /// Spawn a tab. An absent focused session does not fall back to another session.
    pub fn new_terminal(&mut self, connection: &Connection, cwd: Option<&str>) {
        self.request(connection, Destination::Tab, cwd, None);
    }

    /// Spawn a tab in `session_id`. Does not substitute another session.
    pub fn new_terminal_in(&mut self, connection: &Connection, session_id: u32, cwd: Option<&str>) {
        self.request(connection, Destination::Tab, cwd, Some(session_id));
    }

    /// Queue an agent capture using the normal acknowledged spawn path.
    /// Background captures remain in server topology, without a local placement.
    pub(crate) fn launch_capture(
        &mut self,
        connection: &Connection,
        request: SpawnRequest,
        follow: bool,
    ) -> Result<u32, crate::connection::CommandError> {
        let id = connection.spawn(request)?;
        self.spawns.insert(
            id,
            if follow {
                Destination::Tab
            } else {
                Destination::Background
            },
        );
        Ok(id)
    }

    /// Open an explicit interactive setup terminal without reconnect replay.
    pub(crate) fn launch_setup(
        &mut self,
        connection: &Connection,
        request: SpawnRequest,
    ) -> Result<u32, crate::connection::CommandError> {
        let id = connection.spawn_once(request)?;
        self.spawns.insert(id, Destination::Tab);
        Ok(id)
    }

    /// The active tab id and its focused placement.
    pub(super) fn active_focus(&self) -> Option<(String, Placement)> {
        Some((self.active_tab()?.id.clone(), self.focused()?.clone()))
    }

    /// Split the focused pane. Without one, this is a new tab.
    pub fn split(&mut self, connection: &Connection, axis: Axis, before: bool) {
        let Some((tab_id, target)) = self.active_focus() else {
            self.new_terminal(connection, None);
            return;
        };
        let cwd = self
            .focused_pane(connection)
            .and_then(|pane| pane.cwd.clone());
        let destination = Destination::Split {
            tab_id,
            target_id: target.id,
            axis,
            before,
        };
        self.request(connection, destination, cwd.as_deref(), None);
    }

    /// Another view of the focused terminal, beside it. Ends zoom.
    pub fn duplicate_view(&mut self, connection: &Connection) {
        let Some((tab_id, target)) = self.active_focus() else {
            return;
        };
        let placement = self.place(connection, &target.terminal_label, None);
        self.claim(&placement);
        if let Some(tab) = self.tab_mut(&tab_id) {
            insert_beside(tab, &target.id, placement, Axis::Row, false);
        }
    }

    /// End the terminal shown by a placement. Keep its views until the server
    /// confirms closure, so a refused kill never silently hides a live shell.
    pub fn terminate_pane(&mut self, connection: &Connection, placement_id: &str) {
        let terminal = self
            .all_placements()
            .find(|placement| placement.id == placement_id)
            .and_then(|placement| placement.terminal_id.clone());
        if let Some(terminal) = terminal {
            if self.dismiss_unavailable(connection, placement_id, &terminal) {
                return;
            }
            self.terminate(connection, &terminal);
        } else {
            self.close_pane(connection, Some(placement_id));
        }
    }

    /// End every distinct terminal in a tab, not only its focused split.
    pub fn terminate_tab(&mut self, connection: &Connection, id: &str) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return;
        };
        let leaves: Vec<_> = placements(&tab.root).into_iter().cloned().collect();
        let mut seen = HashSet::new();
        for placement in leaves {
            if let Some(terminal) = &placement.terminal_id
                && !self.unresolved.contains(&placement.id)
                && !seen.insert(terminal.clone())
            {
                continue;
            }
            self.terminate_pane(connection, &placement.id);
        }
    }

    /// Remove a saved placeholder that the attached server cannot identify.
    /// This is explicitly view removal, not a claim that a process was killed.
    pub(super) fn dismiss_unavailable(
        &mut self,
        connection: &Connection,
        id: &str,
        terminal: &ResourceId,
    ) -> bool {
        if !self.unresolved.contains(id)
            || connection.status != Status::Attached
            || !connection.session_validated()
            || connection.pane(terminal).is_some()
            || connection.ready(terminal)
        {
            return false;
        }
        self.close_pane(connection, Some(id));
        self.error(
            "Unavailable view removed",
            "No live process could be identified for this saved pane. No process was killed."
                .to_owned(),
        );
        true
    }

    /// Release one view for a move. Does not terminate the process.
    pub fn close_pane(&mut self, connection: &Connection, placement_id: Option<&str>) {
        let Some(id) = placement_id
            .map(str::to_owned)
            .or_else(|| self.active_tab().map(|tab| tab.focused_id.clone()))
        else {
            return;
        };
        let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| find_placement(&tab.root, &id).is_some())
        else {
            return;
        };
        let Some(placement) = find_placement(&tab.root, &id).cloned() else {
            return;
        };
        let tab_id = tab.id.clone();
        let Some(next) = remove_leaves(tab.clone(), &HashSet::from([id])) else {
            self.close_tab(connection, &tab_id);
            return;
        };
        self.release(connection, [&placement]);
        if let Some(tab) = self.tab_mut(&tab_id) {
            *tab = next;
        }
    }

    /// Focus an existing leaf showing `terminal`. Returns whether one was found.
    pub fn reveal(&mut self, terminal: &str) -> bool {
        let found = self.tabs.iter().find_map(|tab| {
            placements(&tab.root)
                .into_iter()
                .find(|placement| {
                    placement.terminal_label == terminal
                        || placement
                            .terminal_id
                            .as_ref()
                            .is_some_and(|id| terminal_label(id) == terminal)
                })
                .map(|placement| (tab.id.clone(), placement.id.clone()))
        });
        let Some((tab_id, placement_id)) = found else {
            return false;
        };
        self.focus_placement(&tab_id, &placement_id);
        true
    }

    /// Open a topology pane, attaching it without a geometry vote when needed.
    pub fn open(&mut self, connection: &Connection, pane: &PaneDescriptor) {
        if !self.reveal(&terminal_label(&pane.terminal_id))
            && self.tab_when_ready(connection, &pane.terminal_id)
        {
            let _ = connection
                .client
                .attach_terminal_preserving_geometry(&pane.terminal_id);
        }
    }

    /// Land a ready terminal in a new tab, or await it. Returns whether it waits.
    pub(super) fn tab_when_ready(
        &mut self,
        connection: &Connection,
        terminal: &ResourceId,
    ) -> bool {
        let label = terminal_label(terminal);
        if connection.ready(terminal) {
            self.land(connection, &label, Destination::Tab);
            return false;
        }
        self.awaiting.insert(label, Destination::Tab);
        true
    }

    /// The only path that asks the server to end a process. No retry.
    pub fn terminate(&mut self, connection: &Connection, terminal: &ResourceId) {
        let Some(identity) = connection.identity() else {
            self.error("Terminate refused", "Not connected.".to_owned());
            return;
        };
        if let Err(error) = connection.terminate(&identity, terminal) {
            self.error(error.title, error.body);
        }
    }

    /// Release every view and forget pending work. Does not clear a remembered restore.
    pub fn release_all(&mut self, connection: &Connection) {
        let tabs = std::mem::take(&mut self.tabs);
        self.release(
            connection,
            tabs.iter().flat_map(|tab| placements(&tab.root)),
        );
        self.spawns.clear();
        self.awaiting.clear();
        self.unresolved.clear();
        self.attaching.clear();
        self.closed_terminals.clear();
        self.considered.clear();
        self.owners.clear();
        self.active_id.clear();
    }

    /// How many leaves show `terminal`.
    #[must_use]
    pub fn views_of(&self, terminal: &ResourceId) -> usize {
        let label = terminal_label(terminal);
        self.all_placements()
            .filter(|placement| placement.terminal_label == label)
            .count()
    }

    /// Whether this visible placement proposes the terminal's PTY size.
    #[must_use]
    pub fn size_owner(&self, placement: &Placement) -> bool {
        let Some(tab) = self.active_tab() else {
            return false;
        };
        let siblings: Vec<&Placement> = placements(&tab.root)
            .into_iter()
            .filter(|item| {
                tab.zoomed_id
                    .as_ref()
                    .is_none_or(|zoomed| &item.id == zoomed)
            })
            .filter(|item| item.terminal_label == placement.terminal_label)
            .collect();
        let claimed = self
            .owners
            .get(&placement.terminal_label)
            .and_then(|id| siblings.iter().find(|item| &item.id == id));
        claimed
            .or_else(|| siblings.first())
            .is_some_and(|item| item.id == placement.id)
    }
}
