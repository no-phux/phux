//! Restore saved placement trees and acquire their runtime views.
use super::*;

impl Workspace {
    pub(super) fn finish_restore(&mut self, connection: &Connection) {
        let Some(server) = connection.server_id() else {
            return;
        };
        self.restored = true;
        let layout = self
            .restoring
            .take()
            .filter(|layout| layout.server_id == server);
        let Some(layout) = layout else {
            self.open_initial(connection);
            return;
        };
        let tabs: Vec<DeskTab> = layout
            .tabs
            .iter()
            .filter_map(|tab| self.restore_tab(connection, tab))
            .collect();
        self.active_id = tabs
            .iter()
            .find(|tab| Some(tab.id.as_str()) == layout.active_tab.as_deref())
            .or_else(|| tabs.first())
            .map(|tab| tab.id.clone())
            .unwrap_or_default();
        self.tabs = tabs;
        // An explicit project open may have reseeded a shell since this
        // window last saved its layout. Keep the splits, but show that
        // shell too (or reveal its existing placement).
        if let Some(initial) = self.options.initial_terminal.as_ref()
            && let Some(pane) = connection.pane(initial).cloned()
        {
            self.open(connection, &pane);
        }
    }

    pub(super) fn restore_tab(
        &mut self,
        connection: &Connection,
        saved: &SavedTab,
    ) -> Option<DeskTab> {
        let root = self.rebuild(connection, &saved.root)?;
        let items = placements(&root);
        let focus = items
            .iter()
            .find(|placement| Some(placement.id.as_str()) == saved.focused_id.as_deref())
            .or_else(|| {
                items
                    .iter()
                    .find(|placement| placement.terminal_label == saved.focused_terminal)
            })
            .or_else(|| items.first())
            .copied();
        if let Some(focus) = focus {
            self.claim(focus);
        }
        Some(DeskTab {
            id: saved.id.clone(),
            title: saved.title.clone(),
            focused_id: focus
                .map(|placement| placement.id.clone())
                .unwrap_or_default(),
            zoomed_id: None,
            root,
        })
    }

    pub(super) fn open_initial(&mut self, connection: &Connection) {
        if let Some(initial) = self.options.initial_terminal.as_ref()
            && let Some(pane) = connection.pane(initial).cloned()
        {
            self.open(connection, &pane);
            return;
        }
        if self.options.fresh {
            self.request(connection, Destination::Tab, None, None);
            return;
        }
        let home = connection
            .panes()
            .iter()
            .find(|pane| pane.session_name == self.session)
            .map(|pane| pane.terminal_id.clone());
        if let Some(home) = home {
            self.tab_when_ready(connection, &home);
        } else if !self.preserve_empty {
            self.request(connection, Destination::Tab, None, None);
        }
    }

    pub(super) fn rebuild(
        &mut self,
        connection: &Connection,
        node: &SavedNode,
    ) -> Option<LayoutNode> {
        match node {
            SavedNode::Leaf { id, terminal_id } => {
                if self.closed_terminals.contains(terminal_id) {
                    return None;
                }
                Some(leaf(self.place(connection, terminal_id, id.clone())))
            }
            SavedNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let first = self.rebuild(connection, first);
                let second = self.rebuild(connection, second);
                match (first, second) {
                    (None, other) | (other, None) => other,
                    (Some(first), Some(second)) => Some(LayoutNode::Split {
                        id: new_id("split"),
                        axis: *axis,
                        ratio: *ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                }
            }
        }
    }

    pub(super) fn resolve_restored(&mut self, connection: &Connection) {
        if self.unresolved.is_empty() {
            return;
        }
        let live: HashSet<String> = connection
            .panes()
            .iter()
            .map(|pane| terminal_label(&pane.terminal_id))
            .collect();
        let mut tabs = std::mem::take(&mut self.tabs);
        for placement in tabs
            .iter_mut()
            .flat_map(|tab| placements_mut(&mut tab.root))
        {
            self.resolve(connection, &live, placement);
        }
        self.tabs = tabs;
    }

    /// Give an unresolved placement its view once its terminal is ready.
    pub(super) fn resolve(
        &mut self,
        connection: &Connection,
        live: &HashSet<String>,
        placement: &mut Placement,
    ) {
        if !self.unresolved.contains(&placement.id) {
            return;
        }
        let Some(terminal) = placement.terminal_id.clone() else {
            return;
        };
        if !connection.ready(&terminal) {
            if live.contains(&placement.terminal_label)
                && self.attaching.insert(placement.terminal_label.clone())
            {
                let _ = connection
                    .client
                    .attach_terminal_preserving_geometry(&terminal);
            }
            return;
        }
        if let Some(view) = self.view_for(connection, &terminal, &placement.id) {
            self.unresolved.remove(&placement.id);
            placement.view_id = Some(view);
        }
    }

    /// A view of a ready terminal. A failure is noticed once per placement.
    pub(super) fn view_for(
        &mut self,
        connection: &Connection,
        terminal: &ResourceId,
        placement_id: &str,
    ) -> Option<ViewId> {
        let view = connection.client.create_view(terminal);
        log::debug!("create view for {terminal:?}: {view:?}");
        match view {
            Ok(view) => Some(view),
            Err(error) => {
                if self.noticed_create.insert(placement_id.to_owned()) {
                    self.error("Could not open terminal", error.to_string());
                }
                None
            }
        }
    }
}
