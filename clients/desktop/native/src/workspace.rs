//! Workspace state over one connection: tabs, split trees, focus, and pending
//! terminals. User closes terminate processes; releasing views for a window
//! move or disconnect never does.

pub mod layout;
pub mod persist;

mod operations;
mod restore;

pub use persist::{SavedLayout, layout_paths};

use crate::connection::{Connection, parse_terminal, terminal_label};
use crate::settings::DisplayPrefs;
use layout::{
    Axis, DeskTab, Direction, LayoutNode, Placement, cycle, equalize, find_placement,
    focused_placement, leaf, neighbor, new_id, nudge, placements, placements_mut, refocus,
    remove_leaves, reorder, set_ratio, split_at, step_index, without_terminal,
};
use persist::{SavedNode, SavedTab, save_layout};
use phux_client_core::organization::{self, Place, PlaceSource};
use phux_client_runtime::ViewId;
use phux_client_runtime::control::{Event, PaneDescriptor, SpawnRequest, Status};
use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeature;
use std::collections::{HashMap, HashSet};

/// How a new workspace should open.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceOptions {
    /// Open a fresh terminal instead of the session's home pane.
    pub fresh: bool,
    /// Show this existing terminal. Used by Move Pane to New Window; Move Pane
    /// to Next Window reveals it in an open window instead.
    pub initial_terminal: Option<ResourceId>,
}

/// Where a terminal that is not ready yet will land.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    Tab,
    Background,
    Split {
        tab_id: String,
        target_id: String,
        axis: Axis,
        before: bool,
    },
}

/// A message for the window's notification feed. Workspace command errors reach the feed only through here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// `info` or `error`.
    pub kind: &'static str,
    /// Title.
    pub title: String,
    /// Optional body.
    pub body: Option<String>,
}

/// Tabs, focus, and pending spawns for one window.
#[derive(Default)]
pub struct Workspace {
    /// Open tabs.
    pub tabs: Vec<DeskTab>,
    /// Active tab id. Empty when there are no tabs.
    pub active_id: String,
    session: String,
    options: WorkspaceOptions,
    preserve_empty: bool,
    views_epoch: Option<u64>,
    spawns: HashMap<u32, Destination>,
    awaiting: HashMap<String, Destination>,
    owners: HashMap<String, String>,
    restoring: Option<SavedLayout>,
    restored: bool,
    server_id: Option<String>,
    unresolved: HashSet<String>,
    attaching: HashSet<String>,
    closed_terminals: HashSet<String>,
    notices: Vec<Notice>,
    noticed_create: HashSet<String>,
}

impl Workspace {
    /// The home remains the same session when its server-side name changes.
    pub(crate) fn set_home_session(&mut self, session: String) {
        self.session = session;
    }

    /// A window's workspace over `session`. Every operation that touches the
    /// runtime takes the window's [`Connection`].
    #[must_use]
    pub fn new(session: String, options: WorkspaceOptions) -> Self {
        Self {
            session,
            options,
            ..Self::default()
        }
    }

    /// Browsing existing work must not create a terminal in an empty session.
    pub(crate) fn preserve_empty(&mut self) {
        self.preserve_empty = true;
    }

    /// The active tab, or the first tab.
    #[must_use]
    pub fn active_tab(&self) -> Option<&DeskTab> {
        self.tabs
            .iter()
            .find(|tab| tab.id == self.active_id)
            .or_else(|| self.tabs.first())
    }

    fn active_tab_mut(&mut self) -> Option<&mut DeskTab> {
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == self.active_id)
            .unwrap_or(0);
        self.tabs.get_mut(index)
    }

    fn tab_mut(&mut self, id: &str) -> Option<&mut DeskTab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    fn all_placements(&self) -> impl Iterator<Item = &Placement> {
        self.tabs.iter().flat_map(|tab| placements(&tab.root))
    }

    /// The focused placement.
    #[must_use]
    pub fn focused(&self) -> Option<&Placement> {
        self.active_tab().and_then(focused_placement)
    }

    fn focused_pane<'a>(&self, connection: &'a Connection) -> Option<&'a PaneDescriptor> {
        connection.pane(self.focused()?.terminal_id.as_ref()?)
    }

    /// Spawns and attaches still waiting for a terminal.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.spawns.len() + self.awaiting.len()
    }

    /// Only an awaited terminal's bootstrap can land pending work. Unrelated
    /// output must not turn a busy background pane into full chrome rebuilds.
    pub(crate) fn awaiting_output(&self, events: &[Event]) -> bool {
        events.iter().any(|event| match event {
            Event::TerminalChanged { terminal_id } => {
                self.awaiting.contains_key(&terminal_label(terminal_id))
                    || (!self.unresolved.is_empty()
                        && self.all_placements().any(|placement| {
                            self.unresolved.contains(&placement.id)
                                && placement.terminal_id.as_ref() == Some(terminal_id)
                        }))
            }
            _ => false,
        })
    }

    /// Incarnation that owns the current tabs.
    #[must_use]
    pub fn server_id(&self) -> Option<&str> {
        self.server_id.as_deref()
    }

    /// Whether the saved layout has been applied (or found absent) for the attached server.
    #[must_use]
    pub fn restored(&self) -> bool {
        self.restored
    }

    /// Take queued notices.
    pub fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    /// Remember a layout until the matching server attaches. Does not spawn.
    pub fn restore(&mut self, layout: Option<SavedLayout>) {
        self.restoring = layout;
        self.restored = false;
    }

    /// Apply spawn, attach, close, and kill events. Does not terminate on close.
    pub fn handle(&mut self, events: &[Event], connection: &Connection) {
        self.sync_server(connection);
        for event in events {
            match event {
                Event::TerminalSpawned {
                    request_id,
                    terminal_id,
                    error,
                } => self.answered(*request_id, terminal_id.as_ref(), error.as_deref()),
                Event::TerminalAttached {
                    terminal_id,
                    error: Some(error),
                    ..
                } => {
                    self.awaiting.remove(&terminal_label(terminal_id));
                    self.error("Could not open terminal", error.clone());
                }
                Event::TerminalClosed { terminal_id, .. } => {
                    self.closed(&terminal_label(terminal_id));
                }
                Event::TerminalKilled {
                    error: Some(error), ..
                } => self.error("Terminate refused", error.clone()),
                _ => {}
            }
        }
    }

    /// Place ready terminals and finish a pending restore. Call after [`Self::handle`].
    pub fn settle(&mut self, connection: &Connection) {
        self.sync_server(connection);
        if connection.status != Status::Attached || !connection.session_validated() {
            return;
        }
        self.rearm_views(connection);
        let ready: Vec<(String, Destination)> = self
            .awaiting
            .extract_if(|label, _| parse_terminal(label).is_some_and(|id| connection.ready(&id)))
            .collect();
        for (label, destination) in ready {
            self.place_ready(connection, &label, destination);
        }
        if !self.restored {
            self.finish_restore(connection);
        }
        self.resolve_restored(connection);
    }

    /// Place one terminal the server reports ready at its recorded
    /// destination. A tab spawn whose terminal is already shown is revealed
    /// instead of landing twice; a split always lands in its own tab.
    fn place_ready(&mut self, connection: &Connection, label: &str, destination: Destination) {
        self.awaiting.remove(label);
        let revealed = self.reveal(label);
        if !revealed || matches!(destination, Destination::Split { .. }) {
            self.land(connection, label, destination);
        }
    }

    /// A validated new connection must restore visible foreign-session streams too.
    /// Existing views keep their last frame while admission/bootstrap is renewed.
    fn rearm_views(&mut self, connection: &Connection) {
        // An acknowledged spawn can reconnect before it has a placement.
        // Its exact id is known even when the home snapshot omits the foreign
        // session. Runtime admission deduplicates this without respawning.
        for terminal in self
            .awaiting
            .keys()
            .filter_map(|label| parse_terminal(label))
        {
            let _ = connection
                .client
                .attach_terminal_preserving_geometry(&terminal);
        }
        let epoch = connection.connection_epoch();
        if self.views_epoch == Some(epoch) {
            return;
        }
        self.views_epoch = Some(epoch);
        for terminal in self
            .all_placements()
            .filter_map(|placement| placement.terminal_id.as_ref())
        {
            if connection.pane(terminal).is_some() {
                let _ = connection
                    .client
                    .attach_terminal_preserving_geometry(terminal);
            }
        }
    }

    fn request(
        &mut self,
        connection: &Connection,
        destination: Destination,
        cwd: Option<&str>,
        session_id: Option<u32>,
    ) {
        let focused = self.focused_pane(connection);
        let place = organization::place_for(
            Some(&Place {
                host: None,
                directory: focused.and_then(|pane| pane.cwd.clone()),
            }),
            PlaceSource::Explicit {
                host: None,
                directory: cwd.map(str::to_owned),
            },
        );
        let mut request = SpawnRequest {
            cwd: place.directory,
            session_id: session_id
                .or_else(|| focused.map(|pane| pane.session_id))
                .or_else(|| connection.home_session()),
            ..SpawnRequest::default()
        };
        if let Some(size) = self.predicted_size(connection, &destination)
            && size.0 >= 2
            && size.1 >= 1
        {
            request.initial_size = Some(size);
        }
        match connection.spawn(request) {
            Ok(id) => {
                self.spawns.insert(id, destination);
            }
            Err(error) => self.error(error.title, error.body),
        }
    }

    fn predicted_size(
        &self,
        connection: &Connection,
        destination: &Destination,
    ) -> Option<(u16, u16)> {
        if !connection
            .server
            .as_ref()
            .is_some_and(|server| server.has(ServerFeature::SpawnInitialSize))
        {
            return None;
        }
        let frame = connection.client.acquire_view(self.focused()?.view_id?)?;
        let (cols, rows) = (frame.cols, frame.rows);
        if cols < 4 || rows < 4 {
            return None;
        }
        let Destination::Split { axis, .. } = destination else {
            return Some((cols, rows));
        };
        let header = if self
            .active_tab()
            .is_some_and(|tab| placements(&tab.root).len() == 1)
        {
            2
        } else {
            0
        };
        Some(match axis {
            Axis::Row => ((cols.saturating_sub(1)) / 2, rows.saturating_sub(header)),
            Axis::Column => (cols, ((rows.saturating_sub(1)) / 2).saturating_sub(header)),
        })
    }

    fn answered(&mut self, request_id: u32, terminal: Option<&ResourceId>, error: Option<&str>) {
        let Some(destination) = self.spawns.remove(&request_id) else {
            return;
        };
        match (terminal, error) {
            (Some(terminal), None) => {
                if destination != Destination::Background {
                    self.awaiting.insert(terminal_label(terminal), destination);
                }
            }
            _ => self.error(
                "Could not start a terminal",
                error.unwrap_or("The server named no terminal.").to_owned(),
            ),
        }
    }

    fn closed(&mut self, label: &str) {
        self.closed_terminals.insert(label.to_owned());
        self.awaiting.remove(label);
        let doomed: Vec<String> = self
            .all_placements()
            .filter(|placement| placement.terminal_label == label)
            .map(|placement| placement.id.clone())
            .collect();
        if doomed.is_empty() {
            return;
        }
        for id in &doomed {
            self.unresolved.remove(id);
        }
        self.tabs = without_terminal(std::mem::take(&mut self.tabs), label);
        if !self.tabs.iter().any(|tab| tab.id == self.active_id) {
            self.active_id = self
                .tabs
                .first()
                .map(|tab| tab.id.clone())
                .unwrap_or_default();
        }
    }

    fn land(&mut self, connection: &Connection, label: &str, destination: Destination) {
        let placement = self.place(connection, label, None);
        self.claim(&placement);
        if let Destination::Split {
            tab_id,
            target_id,
            axis,
            before,
        } = &destination
            && let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|tab| &tab.id == tab_id && find_placement(&tab.root, target_id).is_some())
        {
            insert_beside(tab, target_id, placement, *axis, *before);
            self.active_id.clone_from(tab_id);
            return;
        }
        let id = new_id("tab");
        self.tabs.push(DeskTab {
            id: id.clone(),
            title: None,
            focused_id: placement.id.clone(),
            root: leaf(placement),
            zoomed_id: None,
        });
        self.active_id = id;
    }

    fn place(&mut self, connection: &Connection, label: &str, id: Option<String>) -> Placement {
        let id = id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| new_id("place"));
        let terminal_id = parse_terminal(label);
        let view_id = terminal_id
            .as_ref()
            .filter(|terminal| connection.ready(terminal))
            .and_then(|terminal| self.view_for(connection, terminal, &id));
        if view_id.is_none() {
            self.unresolved.insert(id.clone());
        }
        Placement {
            id,
            terminal_id,
            terminal_label: label.to_owned(),
            view_id,
        }
    }

    fn claim(&mut self, placement: &Placement) {
        self.owners
            .insert(placement.terminal_label.clone(), placement.id.clone());
    }

    fn release<'a>(
        &mut self,
        connection: &Connection,
        placements: impl IntoIterator<Item = &'a Placement>,
    ) {
        for placement in placements {
            self.unresolved.remove(&placement.id);
            if let Some(view) = placement.view_id {
                // A view of a closed client died with it; there is nothing to release.
                let _ = connection.client.destroy_view(view);
            }
        }
    }

    fn sync_server(&mut self, connection: &Connection) {
        let Some(current) = connection.server_id() else {
            return;
        };
        if self.server_id.as_deref() == Some(current.as_str()) {
            return;
        }
        if self.server_id.is_some() {
            self.release_all(connection);
            self.restoring = None;
            self.restored = false;
        }
        self.server_id = Some(current);
    }

    fn error(&mut self, title: &str, body: String) {
        self.notices.push(Notice {
            kind: "error",
            title: title.to_owned(),
            body: Some(body),
        });
    }
}

/// Put `placement` beside `target_id` and focus it. Ends zoom.
fn insert_beside(
    tab: &mut DeskTab,
    target_id: &str,
    placement: Placement,
    axis: Axis,
    before: bool,
) {
    tab.zoomed_id = None;
    tab.focused_id = placement.id.clone();
    split_at(&mut tab.root, target_id, placement, axis, before);
}

/// Save the current tabs when the incarnation matches. Display-only updates
/// stay with the caller.
#[must_use]
pub fn snapshot_layout(
    workspace: &Workspace,
    connection: &Connection,
    display: &DisplayPrefs,
) -> Option<SavedLayout> {
    let server = workspace.server_id()?;
    if connection.server_id().as_deref() != Some(server) || connection.status != Status::Attached {
        return None;
    }
    Some(save_layout(
        server,
        &workspace.tabs,
        display,
        Some(&workspace.active_id),
    ))
}

/// Refuse a `phux new` that would fall back to the desktop process directory.
#[must_use]
pub fn create_session_error(name: &str, directory: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("A session needs a name.");
    }
    if directory.trim().is_empty() {
        return Some("A new session needs the focused directory or a chosen folder.");
    }
    None
}

#[cfg(test)]
mod close_tests;
#[cfg(test)]
pub(crate) mod view_tests;

#[cfg(test)]
mod home_tests;
