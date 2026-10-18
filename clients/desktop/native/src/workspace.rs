//! Workspace state over one connection: tabs, split trees, focus, and pending
//! terminals. Closing a pane or tab releases views. Only [`Workspace::terminate`]
//! ends a process.

pub mod layout;
pub mod persist;

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

    fn focus_from_active(&mut self, target: impl FnOnce(&LayoutNode, &str) -> Option<String>) {
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

    /// The active tab id and its focused placement.
    fn active_focus(&self) -> Option<(String, Placement)> {
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

    /// Release one view. Does not terminate the process.
    pub fn close_pane(&mut self, connection: &Connection, placement_id: Option<&str>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let id = placement_id.unwrap_or(&tab.focused_id).to_owned();
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
    fn tab_when_ready(&mut self, connection: &Connection, terminal: &ResourceId) -> bool {
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

    fn finish_restore(&mut self, connection: &Connection) {
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

    fn restore_tab(&mut self, connection: &Connection, saved: &SavedTab) -> Option<DeskTab> {
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

    fn open_initial(&mut self, connection: &Connection) {
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

    fn rebuild(&mut self, connection: &Connection, node: &SavedNode) -> Option<LayoutNode> {
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

    fn resolve_restored(&mut self, connection: &Connection) {
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
    fn resolve(
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
    fn view_for(
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
pub(crate) mod view_tests;

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
        let placement = Placement {
            id: "restored-placement".into(),
            terminal_id: Some(ResourceId::local(3)),
            terminal_label: "@3".into(),
            view_id: None,
        };
        workspace.unresolved.insert(placement.id.clone());
        workspace.tabs.push(DeskTab {
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

    fn one_pane_tab(tab: &str, terminal: u32) -> DeskTab {
        let placement = Placement {
            id: format!("{tab}-place"),
            terminal_id: Some(ResourceId::local(terminal)),
            terminal_label: terminal_label(&ResourceId::local(terminal)),
            view_id: None,
        };
        DeskTab {
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
            axis: Axis::Row,
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

    fn leaves(tab: &DeskTab) -> Vec<String> {
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
        let before = (workspace.tabs.clone(), workspace.active_id.clone());
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
        assert_eq!(
            (workspace.tabs.clone(), workspace.active_id.clone()),
            before
        );
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
