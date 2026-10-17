//! Workspace state over one connection: tabs, split trees, focus, and pending
//! terminals. Closing a pane or tab releases views. Only [`Workspace::terminate`]
//! ends a process.

pub mod layout;
pub mod persist;
pub mod place;
pub mod session;

pub use layout::{
    Axis, DeskTab, Direction, LayoutNode, Placement, Rect, clamp_ratio, cycle, equalize,
    find_placement, focus_after_removal, focused_placement, layout_rects, leaf, neighbor, new_id,
    nudge, placements, refocus, remove_leaf, reorder, set_ratio, split_at, without_terminal,
};
pub use persist::{
    FileLayoutStore, LayoutPaths, LoadedLayout, SavedLayout, SavedNode, SavedTab, StoreValue,
    layout_paths, layout_value, load_layout, parse_layout, quick_display, save_layout, target_hash,
    write_if_allowed,
};
pub use place::{ProjectGroup, SessionLine, group_by_project, place_for, terminal_host};
pub use session::{
    create_session_args, create_session_error, rename_session_args, rename_session_error,
    window_session,
};

use crate::connection::{Connection, parse_terminal, terminal_label};
use crate::settings::DisplayPrefs;
use layout::{Axis as SplitAxis, DeskTab as Tab, LayoutNode as Node, Placement as LeafPlacement};
use persist::SavedNode as StoredNode;
use phux_client_runtime::control::{Event, PaneDescriptor, SpawnRequest, Status};
use phux_client_runtime::{Client, ViewId};
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
    Split {
        tab_id: String,
        target_id: String,
        axis: SplitAxis,
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
pub struct Workspace {
    /// Open tabs.
    pub tabs: Vec<Tab>,
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
            tabs: Vec::new(),
            active_id: String::new(),
            session,
            options,
            preserve_empty: false,
            views_epoch: None,
            spawns: HashMap::new(),
            awaiting: HashMap::new(),
            owners: HashMap::new(),
            restoring: None,
            restored: false,
            server_id: None,
            unresolved: HashSet::new(),
            attaching: HashSet::new(),
            closed_terminals: HashSet::new(),
            notices: Vec::new(),
            noticed_create: HashSet::new(),
        }
    }

    /// Browsing existing work must not create a terminal in an empty session.
    pub(crate) fn preserve_empty(&mut self) {
        self.preserve_empty = true;
    }

    /// The active tab, or the first tab.
    #[must_use]
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs
            .iter()
            .find(|tab| tab.id == self.active_id)
            .or_else(|| self.tabs.first())
    }

    /// The focused placement.
    #[must_use]
    pub fn focused(&self) -> Option<&LeafPlacement> {
        self.active_tab().and_then(focused_placement)
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
                let label = terminal_label(terminal_id);
                self.awaiting.contains_key(&label) || self.awaiting_view(terminal_id)
            }
            _ => false,
        })
    }

    fn awaiting_view(&self, terminal: &ResourceId) -> bool {
        if self.unresolved.is_empty() {
            return false;
        }
        self.tabs
            .iter()
            .flat_map(|tab| placements(&tab.root))
            .any(|placement| {
                self.unresolved.contains(&placement.id)
                    && placement.terminal_id.as_ref() == Some(terminal)
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
            if let Event::TerminalSpawned {
                request_id,
                terminal_id,
                error,
            } = event
            {
                self.answered(*request_id, terminal_id.as_ref(), error.as_deref());
            }
            if let Event::TerminalAttached {
                terminal_id,
                error: Some(error),
                ..
            } = event
            {
                self.awaiting.remove(&terminal_label(terminal_id));
                self.notice("error", "Could not open terminal", Some(error.clone()));
            }
            if let Event::TerminalClosed { terminal_id, .. } = event {
                self.closed(&terminal_label(terminal_id));
            }
            if let Event::TerminalKilled {
                error: Some(error), ..
            } = event
            {
                self.notice("error", "Terminate refused", Some(error.clone()));
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
            .iter()
            .filter(|(label, _)| parse_terminal(label).is_some_and(|id| connection.ready(&id)))
            .map(|(label, destination)| (label.clone(), destination.clone()))
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
        let epoch = connection.connection_epoch();
        if self.views_epoch == Some(epoch) {
            return;
        }
        self.views_epoch = Some(epoch);
        for terminal in self
            .tabs
            .iter()
            .flat_map(|tab| placements(&tab.root))
            .filter_map(|placement| placement.terminal_id.as_ref())
        {
            if connection.pane(terminal).is_some() {
                let _ = connection
                    .client
                    .attach_terminal_preserving_geometry(terminal);
            }
        }
    }

    /// Select a tab that exists.
    pub fn select_tab(&mut self, id: &str) {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.active_id = id.to_owned();
        }
    }

    /// Move the active tab by `step`, wrapping.
    pub fn step_tab(&mut self, step: i8) {
        let Some(current) = self.active_tab().map(|tab| tab.id.clone()) else {
            return;
        };
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == current)
            .unwrap_or(0) as i32;
        let len = self.tabs.len() as i32;
        if len == 0 {
            return;
        }
        let next = (index + i32::from(step)).rem_euclid(len) as usize;
        if let Some(tab) = self.tabs.get(next) {
            let id = tab.id.clone();
            self.select_tab(&id);
        }
    }

    /// Select by index. A negative index selects the last tab.
    pub fn select_index(&mut self, index: isize) {
        let id = if index < 0 {
            self.tabs.last().map(|tab| tab.id.clone())
        } else {
            self.tabs.get(index as usize).map(|tab| tab.id.clone())
        };
        if let Some(id) = id {
            self.select_tab(&id);
        }
    }

    /// Reorder tabs.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        self.tabs = reorder(std::mem::take(&mut self.tabs), from, to);
    }

    /// Set or clear a tab title. Blank clears it. Titles are at most 80 characters.
    pub fn rename_tab(&mut self, id: &str, title: &str) {
        let title = title.trim();
        let title = if title.is_empty() {
            None
        } else {
            Some(title.chars().take(80).collect())
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.title = title;
            *tab = refocus(tab.clone());
        }
    }

    /// Release the tab's views. Does not terminate processes.
    pub fn close_tab(&mut self, connection: &Connection, id: &str) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let tab = self.tabs.remove(index);
        self.release(
            connection,
            &placements(&tab.root)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
        );
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
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.focused_id = placement_id.to_owned();
            if tab.zoomed_id.is_some() {
                tab.zoomed_id = Some(placement_id.to_owned());
            }
        }
        self.active_id = tab_id.to_owned();
    }

    /// Focus the neighbour in `direction`.
    pub fn focus_direction(&mut self, direction: Direction) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let tab_id = tab.id.clone();
        let focused = tab.focused_id.clone();
        if let Some(target) = neighbor(&tab.root, &focused, direction) {
            self.focus_placement(&tab_id, &target);
        }
    }

    /// Cycle panes, wrapping.
    pub fn cycle_pane(&mut self, step: i8) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let tab_id = tab.id.clone();
        let focused = tab.focused_id.clone();
        if let Some(target) = cycle(&tab.root, &focused, step) {
            self.focus_placement(&tab_id, &target);
        }
    }

    /// Zoom the focused pane, or clear zoom. A single pane does not zoom.
    pub fn toggle_zoom(&mut self) {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id.clone()) else {
            return;
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            if placements(&tab.root).len() < 2 {
                return;
            }
            if tab.zoomed_id.take().is_none() {
                tab.zoomed_id = Some(tab.focused_id.clone());
            }
        }
    }

    /// Nudge the nearest divider by Ghostty's step.
    pub fn nudge(&mut self, direction: Direction) {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id.clone()) else {
            return;
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            let focused = tab.focused_id.clone();
            tab.root = nudge(
                std::mem::replace(&mut tab.root, empty_leaf()),
                &focused,
                direction,
                0.04,
            );
            *tab = refocus(tab.clone());
        }
    }

    /// Equalize the active tab.
    pub fn equalize_tab(&mut self) {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id.clone()) else {
            return;
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.root = equalize(std::mem::replace(&mut tab.root, empty_leaf()));
        }
    }

    /// Set one split ratio in the active tab.
    pub fn resize_split(&mut self, split_id: &str, ratio: f32) {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id.clone()) else {
            return;
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.root = set_ratio(
                std::mem::replace(&mut tab.root, empty_leaf()),
                split_id,
                ratio,
            );
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

    /// Split the focused pane. Without one, this is a new tab.
    pub fn split(&mut self, connection: &Connection, axis: SplitAxis, before: bool) {
        let Some(tab) = self.active_tab() else {
            self.new_terminal(connection, None);
            return;
        };
        let Some(target) = self.focused().cloned() else {
            self.new_terminal(connection, None);
            return;
        };
        let cwd = self.focused_cwd(connection);
        self.request(
            connection,
            Destination::Split {
                tab_id: tab.id.clone(),
                target_id: target.id,
                axis,
                before,
            },
            cwd.as_deref(),
            None,
        );
    }

    /// Another view of the focused terminal, beside it. Ends zoom.
    pub fn duplicate_view(&mut self, connection: &Connection) {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id.clone()) else {
            return;
        };
        let Some(target) = self.focused().cloned() else {
            return;
        };
        let placement = self.place(connection, &target.terminal_label, None);
        self.claim(&placement);
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.zoomed_id = None;
            let id = placement.id.clone();
            tab.root = split_at(
                std::mem::replace(&mut tab.root, empty_leaf()),
                &target.id,
                placement,
                SplitAxis::Row,
                false,
            );
            tab.focused_id = id;
            *tab = refocus(tab.clone());
        }
    }

    /// Release one view. Does not terminate the process.
    pub fn close_pane(&mut self, connection: &Connection, placement_id: Option<&str>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let id = placement_id
            .map(str::to_owned)
            .unwrap_or_else(|| tab.focused_id.clone());
        let tab_id = tab.id.clone();
        let Some(placement) = find_placement(&tab.root, &id).cloned() else {
            return;
        };
        let order: Vec<String> = placements(&tab.root)
            .into_iter()
            .map(|item| item.id.clone())
            .collect();
        self.release(connection, &[placement]);
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        let Some(root) = remove_leaf(std::mem::replace(&mut tab.root, empty_leaf()), &id) else {
            self.close_tab(connection, &tab_id);
            return;
        };
        let focused = focus_after_removal(&order, &tab.focused_id, &HashSet::from([id]));
        tab.root = root;
        tab.focused_id = focused;
        *tab = refocus(tab.clone());
    }

    /// Focus an existing leaf showing `terminal`. Returns whether one was found.
    pub fn reveal(&mut self, terminal: &str) -> bool {
        let found = self.tabs.iter().find_map(|tab| {
            placements(&tab.root).into_iter().find_map(|placement| {
                (placement.terminal_label == terminal
                    || placement
                        .terminal_id
                        .as_ref()
                        .is_some_and(|id| terminal_label(id) == terminal))
                .then(|| (tab.id.clone(), placement.id.clone()))
            })
        });
        if let Some((tab_id, placement_id)) = found {
            self.focus_placement(&tab_id, &placement_id);
            true
        } else {
            false
        }
    }

    /// Open a topology pane, attaching it without a geometry vote when needed.
    pub fn open(&mut self, connection: &Connection, pane: &PaneDescriptor) {
        let label = terminal_label(&pane.terminal_id);
        if self.reveal(&label) {
            return;
        }
        if connection.ready(&pane.terminal_id) {
            self.land(connection, &label, Destination::Tab);
            return;
        }
        let _ = connection
            .client
            .attach_terminal_preserving_geometry(&pane.terminal_id);
        self.awaiting.insert(label, Destination::Tab);
    }

    /// The only path that asks the server to end a process. No retry.
    pub fn terminate(&mut self, connection: &Connection, terminal: &ResourceId) {
        let Some(identity) = connection.identity() else {
            self.notice(
                "error",
                "Terminate refused",
                Some("Not connected.".to_owned()),
            );
            return;
        };
        if let Err(error) = connection.terminate(&identity, terminal) {
            self.notice("error", error.title, Some(error.body));
        }
    }

    /// Tabs and the active id, for persistence.
    #[must_use]
    pub fn snapshot(&self) -> (Vec<Tab>, String) {
        (self.tabs.clone(), self.active_id.clone())
    }

    /// Release every view and forget pending work. Does not clear a remembered restore.
    pub fn release_all(&mut self, connection: &Connection) {
        let placements = self
            .tabs
            .iter()
            .flat_map(|tab| placements(&tab.root).into_iter().cloned())
            .collect::<Vec<_>>();
        self.release(connection, &placements);
        self.spawns.clear();
        self.awaiting.clear();
        self.unresolved.clear();
        self.attaching.clear();
        self.closed_terminals.clear();
        self.owners.clear();
        self.tabs.clear();
        self.active_id.clear();
    }

    /// How many leaves show `terminal`.
    #[must_use]
    pub fn views_of(&self, terminal: &ResourceId) -> usize {
        let label = terminal_label(terminal);
        self.tabs
            .iter()
            .map(|tab| {
                placements(&tab.root)
                    .into_iter()
                    .filter(|placement| placement.terminal_label == label)
                    .count()
            })
            .sum()
    }

    /// Whether this visible placement proposes the terminal's PTY size.
    #[must_use]
    pub fn size_owner(&self, placement: &LeafPlacement) -> bool {
        let Some(tab) = self.active_tab() else {
            return false;
        };
        let visible: Vec<&LeafPlacement> = if let Some(zoomed) = &tab.zoomed_id {
            placements(&tab.root)
                .into_iter()
                .filter(|item| &item.id == zoomed)
                .collect()
        } else {
            placements(&tab.root)
        };
        let siblings: Vec<&LeafPlacement> = visible
            .into_iter()
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

    fn request(
        &mut self,
        connection: &Connection,
        destination: Destination,
        cwd: Option<&str>,
        session_id: Option<u32>,
    ) {
        let mut request = SpawnRequest::default();
        let focused = self.focused_cwd(connection);
        let place = place_for(
            focused
                .as_deref()
                .map(|directory| phux_client_core::organization::Place {
                    host: None,
                    directory: Some(directory.to_owned()),
                })
                .as_ref(),
            cwd.map(|directory| phux_client_core::organization::Place {
                host: None,
                directory: Some(directory.to_owned()),
            })
            .as_ref(),
        );
        if let Some(directory) = place.directory {
            request.cwd = Some(directory);
        }
        let inherited = session_id.or_else(|| self.focused_session(connection));
        request.session_id = inherited.or_else(|| connection.home_session());
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
            Err(error) => self.notice("error", error.title, Some(error.body)),
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
        let view = self.focused()?.view_id?;
        let (cols, rows) = view_cells(&connection.client, view)?;
        if cols < 4 || rows < 4 {
            return None;
        }
        if matches!(destination, Destination::Tab) {
            return Some((cols, rows));
        }
        let header = if self
            .active_tab()
            .is_some_and(|tab| placements(&tab.root).len() == 1)
        {
            2
        } else {
            0
        };
        let axis = match destination {
            Destination::Split { axis, .. } => *axis,
            Destination::Tab => return Some((cols, rows)),
        };
        Some(match axis {
            SplitAxis::Row => ((cols.saturating_sub(1)) / 2, rows.saturating_sub(header)),
            SplitAxis::Column => (cols, ((rows.saturating_sub(1)) / 2).saturating_sub(header)),
        })
    }

    fn focused_cwd(&self, connection: &Connection) -> Option<String> {
        let placement = self.focused()?;
        let id = placement.terminal_id.as_ref()?;
        connection.pane(id)?.cwd.clone()
    }

    fn focused_session(&self, connection: &Connection) -> Option<u32> {
        let placement = self.focused()?;
        let id = placement.terminal_id.as_ref()?;
        Some(connection.pane(id)?.session_id)
    }

    fn answered(&mut self, request_id: u32, terminal: Option<&ResourceId>, error: Option<&str>) {
        let Some(destination) = self.spawns.remove(&request_id) else {
            return;
        };
        if error.is_some() || terminal.is_none() {
            self.notice(
                "error",
                "Could not start a terminal",
                Some(error.unwrap_or("The server named no terminal.").to_owned()),
            );
            return;
        }
        if let Some(terminal) = terminal {
            self.awaiting.insert(terminal_label(terminal), destination);
        }
    }

    fn closed(&mut self, label: &str) {
        self.closed_terminals.insert(label.to_owned());
        self.awaiting.remove(label);
        let doomed: Vec<LeafPlacement> = self
            .tabs
            .iter()
            .flat_map(|tab| placements(&tab.root).into_iter().cloned())
            .filter(|placement| placement.terminal_label == label)
            .collect();
        if doomed.is_empty() {
            return;
        }
        self.release_placements(&doomed);
        self.tabs = without_terminal(std::mem::take(&mut self.tabs), label);
        if !self.tabs.iter().any(|tab| tab.id == self.active_id) {
            self.active_id = self
                .tabs
                .first()
                .map(|tab| tab.id.clone())
                .unwrap_or_default();
        }
    }

    fn finish_restore(&mut self, connection: &Connection) {
        let Some(server) = connection.server_id() else {
            return;
        };
        self.restored = true;
        let layout = self
            .restoring
            .take()
            .filter(|layout| layout.server_id == server);
        if let Some(layout) = layout {
            let tabs = layout
                .tabs
                .iter()
                .flat_map(|tab| self.restore_tab(connection, tab))
                .collect::<Vec<_>>();
            let active = tabs
                .iter()
                .find(|tab| Some(tab.id.as_str()) == layout.active_tab.as_deref())
                .or_else(|| tabs.first())
                .map(|tab| tab.id.clone())
                .unwrap_or_default();
            self.tabs = tabs;
            self.active_id = active;
            // An explicit project open may have reseeded a shell since this
            // window last saved its layout. Keep the splits, but show that
            // shell too (or reveal its existing placement).
            if let Some(initial) = self.options.initial_terminal.as_ref()
                && let Some(pane) = connection.pane(initial).cloned()
            {
                self.open(connection, &pane);
            }
            return;
        }
        self.open_initial(connection);
    }

    fn restore_tab(&mut self, connection: &Connection, saved: &persist::SavedTab) -> Option<Tab> {
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
        Some(Tab {
            id: saved.id.clone(),
            title: saved.title.clone(),
            focused_id: focus
                .map(|placement| placement.id.clone())
                .unwrap_or_default(),
            zoomed_id: None,
            root,
        })
    }

    fn open_initial(&mut self, connection: &Connection) {
        if let Some(initial) = self.options.initial_terminal.clone()
            && let Some(pane) = connection.pane(&initial).cloned()
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
            .cloned();
        if let Some(home) = home {
            let label = terminal_label(&home.terminal_id);
            if connection.ready(&home.terminal_id) {
                self.land(connection, &label, Destination::Tab);
            } else {
                self.awaiting.insert(label, Destination::Tab);
            }
            return;
        }
        if !self.preserve_empty {
            self.request(connection, Destination::Tab, None, None);
        }
    }

    fn rebuild(&mut self, connection: &Connection, node: &StoredNode) -> Option<Node> {
        match node {
            StoredNode::Leaf { id, terminal_id } => {
                if self.closed_terminals.contains(terminal_id) {
                    return None;
                }
                Some(leaf(self.place(connection, terminal_id, id.clone())))
            }
            StoredNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let first = self.rebuild(connection, first);
                let second = self.rebuild(connection, second);
                match (first, second) {
                    (None, other) | (other, None) => other,
                    (Some(first), Some(second)) => Some(Node::Split {
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

    fn resolve_restored(&mut self, connection: &Connection) {
        if self.unresolved.is_empty() {
            return;
        }
        let live: HashSet<String> = connection
            .panes()
            .iter()
            .map(|pane| terminal_label(&pane.terminal_id))
            .collect();
        for tab in &mut self.tabs {
            let next = resolve_node(
                &mut self.unresolved,
                &mut self.attaching,
                &mut self.noticed_create,
                &mut self.notices,
                connection,
                &live,
                &tab.root,
            );
            if let Some(root) = next {
                tab.root = root;
            }
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
            && self.destination_alive(tab_id, target_id)
        {
            let id = placement.id.clone();
            if let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.id == tab_id) {
                tab.zoomed_id = None;
                tab.root = split_at(
                    std::mem::replace(&mut tab.root, empty_leaf()),
                    target_id,
                    placement,
                    *axis,
                    *before,
                );
                tab.focused_id = id;
            }
            self.active_id.clone_from(tab_id);
            return;
        }
        let id = new_id("tab");
        let focused = placement.id.clone();
        self.tabs.push(Tab {
            id: id.clone(),
            title: None,
            root: leaf(placement),
            focused_id: focused,
            zoomed_id: None,
        });
        self.active_id = id;
    }

    fn destination_alive(&self, tab_id: &str, target_id: &str) -> bool {
        self.tabs
            .iter()
            .any(|tab| tab.id == tab_id && find_placement(&tab.root, target_id).is_some())
    }

    fn place(&mut self, connection: &Connection, label: &str, id: Option<String>) -> LeafPlacement {
        let id = id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| new_id("place"));
        let terminal_id = parse_terminal(label);
        let view_id = terminal_id.as_ref().and_then(|terminal| {
            if !connection.ready(terminal) {
                return None;
            }
            match create_view(&connection.client, terminal) {
                Ok(view) => Some(view),
                Err(error) => {
                    if self.noticed_create.insert(id.clone()) {
                        self.notices.push(Notice {
                            kind: "error",
                            title: "Could not open terminal".to_owned(),
                            body: Some(error),
                        });
                    }
                    None
                }
            }
        });
        if view_id.is_none() {
            self.unresolved.insert(id.clone());
        }
        LeafPlacement {
            id,
            terminal_id,
            terminal_label: label.to_owned(),
            view_id,
        }
    }

    fn claim(&mut self, placement: &LeafPlacement) {
        self.owners
            .insert(placement.terminal_label.clone(), placement.id.clone());
    }

    fn release(&mut self, connection: &Connection, placements: &[LeafPlacement]) {
        self.release_placements(placements);
        for placement in placements {
            if let Some(view) = placement.view_id {
                destroy_view(&connection.client, view);
            }
        }
    }

    fn release_placements(&mut self, placements: &[LeafPlacement]) {
        for placement in placements {
            self.unresolved.remove(&placement.id);
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
            let placements = self
                .tabs
                .iter()
                .flat_map(|tab| placements(&tab.root).into_iter().cloned())
                .collect::<Vec<_>>();
            self.release(connection, &placements);
            self.spawns.clear();
            self.awaiting.clear();
            self.unresolved.clear();
            self.attaching.clear();
            self.closed_terminals.clear();
            self.owners.clear();
            self.tabs.clear();
            self.active_id.clear();
            self.restoring = None;
            self.restored = false;
        }
        self.server_id = Some(current);
    }

    fn notice(&mut self, kind: &'static str, title: &str, body: Option<String>) {
        self.notices.push(Notice {
            kind,
            title: title.to_owned(),
            body,
        });
    }
}

fn empty_leaf() -> Node {
    leaf(LeafPlacement {
        id: String::new(),
        terminal_id: None,
        terminal_label: String::new(),
        view_id: None,
    })
}

fn resolve_node(
    unresolved: &mut HashSet<String>,
    attaching: &mut HashSet<String>,
    noticed: &mut HashSet<String>,
    notices: &mut Vec<Notice>,
    connection: &Connection,
    live: &HashSet<String>,
    node: &Node,
) -> Option<Node> {
    match node {
        Node::Leaf { placement } => {
            if !unresolved.contains(&placement.id) {
                return None;
            }
            let terminal = placement.terminal_id.clone()?;
            if !connection.ready(&terminal) {
                if live.contains(&placement.terminal_label)
                    && attaching.insert(placement.terminal_label.clone())
                {
                    let _ = connection
                        .client
                        .attach_terminal_preserving_geometry(&terminal);
                }
                return None;
            }
            match create_view(&connection.client, &terminal) {
                Ok(view) => {
                    unresolved.remove(&placement.id);
                    let mut next = placement.clone();
                    next.view_id = Some(view);
                    Some(leaf(next))
                }
                Err(error) => {
                    if noticed.insert(placement.id.clone()) {
                        notices.push(Notice {
                            kind: "error",
                            title: "Could not open terminal".to_owned(),
                            body: Some(error),
                        });
                    }
                    None
                }
            }
        }
        Node::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let next_first = resolve_node(
                unresolved, attaching, noticed, notices, connection, live, first,
            );
            let next_second = resolve_node(
                unresolved, attaching, noticed, notices, connection, live, second,
            );
            if next_first.is_none() && next_second.is_none() {
                return None;
            }
            Some(Node::Split {
                id: id.clone(),
                axis: *axis,
                ratio: *ratio,
                first: Box::new(next_first.unwrap_or_else(|| first.as_ref().clone())),
                second: Box::new(next_second.unwrap_or_else(|| second.as_ref().clone())),
            })
        }
    }
}

fn create_view(client: &Client, terminal: &ResourceId) -> Result<ViewId, String> {
    let view = client
        .create_view(terminal)
        .map_err(|error| error.to_string());
    log::debug!("create view for {terminal:?}: {view:?}");
    view
}

fn destroy_view(client: &Client, view: ViewId) {
    // A view of a closed client died with it; there is nothing to release.
    let _ = client.destroy_view(view);
}

fn view_cells(client: &Client, view: ViewId) -> Option<(u16, u16)> {
    let frame = client.acquire_view(view)?;
    Some((frame.cols, frame.rows))
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
    let (tabs, active) = workspace.snapshot();
    Some(save_layout(server, &tabs, display, Some(active.as_str())))
}

#[cfg(test)]
mod home_tests {
    use super::*;

    #[test]
    fn unrelated_output_does_not_promote_pending_spawns_or_restored_views() {
        let mut workspace = Workspace::new("Intake".to_owned(), WorkspaceOptions::default());
        let event = |id| Event::TerminalChanged {
            terminal_id: ResourceId::local(id),
        };
        workspace.spawns.insert(1, Destination::Tab);
        assert!(!workspace.awaiting_output(&[event(1)]));
        workspace
            .awaiting
            .insert(terminal_label(&ResourceId::local(2)), Destination::Tab);
        assert!(!workspace.awaiting_output(&[event(1)]));
        assert!(workspace.awaiting_output(&[event(2)]));
        let placement = LeafPlacement {
            id: "restored-placement".into(),
            terminal_id: Some(ResourceId::local(3)),
            terminal_label: "@3".into(),
            view_id: None,
        };
        workspace.unresolved.insert(placement.id.clone());
        workspace.tabs.push(Tab {
            id: "tab".into(),
            title: None,
            focused_id: placement.id.clone(),
            zoomed_id: None,
            root: leaf(placement),
        });
        assert!(workspace.awaiting_output(&[event(3)]));
        assert!(!workspace.awaiting_output(&[event(4)]));
        workspace.unresolved.clear();
        assert!(!workspace.awaiting_output(&[event(3)]));
    }

    fn offline() -> (crate::connection::DeterministicDial, Connection) {
        let dial = crate::connection::DeterministicDial::arm();
        let connection =
            Connection::connect_with_key("/isolated-workspace.sock".into(), "main".into(), None)
                .unwrap();
        (dial, connection)
    }

    fn one_pane_tab(tab: &str, terminal: u32) -> Tab {
        let placement = LeafPlacement {
            id: format!("{tab}-place"),
            terminal_id: Some(ResourceId::local(terminal)),
            terminal_label: terminal_label(&ResourceId::local(terminal)),
            view_id: None,
        };
        Tab {
            id: tab.into(),
            title: None,
            focused_id: placement.id.clone(),
            zoomed_id: None,
            root: leaf(placement),
        }
    }

    fn split_of(tab: &str) -> Destination {
        Destination::Split {
            tab_id: tab.into(),
            target_id: format!("{tab}-place"),
            axis: SplitAxis::Row,
            before: false,
        }
    }

    /// One spawn reply followed by the per-terminal step of
    /// [`Workspace::settle`]. `settle` itself returns early on a connection
    /// that is not attached and validated, and no deterministic attached
    /// connection exists for unit tests, so these drive the same
    /// `answered` -> `place_ready` path it runs once the terminal is ready.
    fn reply(workspace: &mut Workspace, connection: &Connection, request: u32, terminal: u32) {
        let terminal = ResourceId::local(terminal);
        workspace.answered(request, Some(&terminal), None);
        let label = terminal_label(&terminal);
        let destination = workspace.awaiting.get(&label).cloned().unwrap();
        workspace.place_ready(connection, &label, destination);
    }

    fn leaves(tab: &Tab) -> Vec<String> {
        placements(&tab.root)
            .into_iter()
            .map(|placement| placement.terminal_label.clone())
            .collect()
    }

    #[test]
    fn pending_split_lands_in_its_original_tab_not_the_later_focus() {
        let (_dial, connection) = offline();
        let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
        workspace.tabs = vec![one_pane_tab("a", 1), one_pane_tab("b", 2)];
        workspace.active_id = "a".into();
        workspace.spawns.insert(7, split_of("a"));
        workspace.select_tab("b");
        reply(&mut workspace, &connection, 7, 9);
        assert_eq!(leaves(&workspace.tabs[0]), ["local:1", "local:9"]);
        assert_eq!(leaves(&workspace.tabs[1]), ["local:2"]);
        assert_eq!(workspace.active_id, "a");
    }

    #[test]
    fn closing_a_pending_split_destination_never_splits_the_focused_pane() {
        let (_dial, connection) = offline();
        let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
        workspace.tabs = vec![one_pane_tab("a", 1), one_pane_tab("b", 2)];
        workspace.active_id = "a".into();
        workspace.spawns.insert(7, split_of("a"));
        workspace.close_tab(&connection, "a");
        assert_eq!(workspace.active_id, "b");
        reply(&mut workspace, &connection, 7, 9);
        // The surviving focused tab is untouched; the orphan gets its own tab
        // instead of being lost (its process is already running).
        assert_eq!(workspace.tabs.len(), 2);
        assert_eq!(leaves(&workspace.tabs[0]), ["local:2"]);
        assert_eq!(leaves(&workspace.tabs[1]), ["local:9"]);
        assert_eq!(workspace.pending(), 0);
    }

    /// The layout is never changed optimistically when a split is requested,
    /// so "restore the previous layout" holds by construction; what a refusal
    /// must get right is dropping the destination so nothing lands later.
    #[test]
    fn refused_split_drops_its_destination_and_keeps_layout_and_focus() {
        let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
        workspace.tabs = vec![one_pane_tab("a", 1), one_pane_tab("b", 2)];
        workspace.active_id = "b".into();
        let before = workspace.snapshot();
        workspace.spawns.insert(7, split_of("b"));
        workspace.answered(7, None, Some("refused"));
        assert_eq!(workspace.pending(), 0);
        let notices = workspace.take_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Could not start a terminal");
        // A late or duplicated reply for the refused request has no
        // destination left, so it cannot split the focused pane.
        workspace.answered(7, Some(&ResourceId::local(9)), None);
        assert!(workspace.awaiting.is_empty());
        assert_eq!(workspace.snapshot(), before);
    }

    #[test]
    fn ready_tab_spawn_for_a_shown_terminal_reveals_instead_of_landing_twice() {
        let (_dial, connection) = offline();
        let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
        workspace.tabs = vec![one_pane_tab("a", 1), one_pane_tab("b", 2)];
        workspace.active_id = "b".into();
        workspace.spawns.insert(7, Destination::Tab);
        reply(&mut workspace, &connection, 7, 1);
        assert_eq!(workspace.tabs.len(), 2);
        assert_eq!(workspace.active_id, "a");
        assert_eq!(workspace.pending(), 0);
    }

    #[test]
    fn home_session_name_tracks_the_same_renamed_session() {
        let mut workspace = Workspace::new("Intake".to_owned(), WorkspaceOptions::default());
        workspace.set_home_session("Unorganized".to_owned());
        assert_eq!(workspace.session, "Unorganized");
        assert!(workspace.tabs.is_empty());
    }
}
