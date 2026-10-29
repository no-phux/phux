//! The window's state: one runtime client, the terminal it shows, and the
//! user's commands against them.
//!
//! The runtime does the terminal (replica, reconnect, acknowledged input,
//! history, selection); this owns only which terminal is on screen, its
//! size, and turning Rune events into runtime calls.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_client_core::layout::{LayoutNode, SplitDir};
use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::control::{Event, ProjectionFence, Status, Topology};
use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client, Listener, ViewId};
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceId};
use rune::{Tx, Waker};

use crate::grid::{self, Metrics};
use crate::launch::Resolved;

mod find;
mod frame;
mod input;
mod layout;
mod links;
mod mouse;
mod palette;
mod sessions;

pub use layout::SplitPath;

/// Wakes Rune's loop from the runtime's driver thread. Only posts; never
/// re-enters the client.
struct Wake(Waker);

impl Listener for Wake {
    fn on_activity(&self) {
        (self.0)();
    }
}

/// How long a requested terminal may stay missing from the server's list
/// before the window gives up on it (a spawn's id lands before its listing).
const WANTED_GRACE: Duration = Duration::from_secs(5);

/// Where Rune keeps this app's settings; the bundle identifier.
const PREFS_ID: &str = "sh.phux.rune";

/// The exact frame a view build painted, and the unknown-delivery fence it
/// may clear once Rune reports it drawn.
pub struct Painted {
    pub terminal: ResourceId,
    pub view: ViewId,
    pub fence: ProjectionFence,
    pub frame: Arc<GridFrame>,
    pub epoch: u64,
}

/// A size this window asked for: a terminal's cells and the cell's device
/// pixels.
type Fitted = ((u16, u16), (u16, u16));

/// What waits on the user's Return or Escape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// A paste that would type line breaks into a program.
    Paste(String),
    /// Closing a terminal that is running a command.
    Close(ResourceId),
}

/// The terminal on screen.
pub struct Shown {
    pub terminal: ResourceId,
    pub view: ViewId,
}

pub struct Desk {
    pub client: Option<Arc<Client>>,
    /// Why there is no client, or the last failure the user should see.
    pub error: Option<String>,
    pub status: Status,
    pub topology: Option<Topology>,
    pub server: Option<String>,
    pub shown: Option<Shown>,
    /// The terminal the user asked for, and since when, until its view opens.
    pub wanted: Option<(ResourceId, Instant)>,
    /// A session this window created: show its terminal once it is selected.
    pending_session: Option<String>,
    /// The connection incarnation the shown view belongs to.
    epoch: u64,
    /// An action waiting for the user's confirmation.
    pub pending: Option<Pending>,
    /// A command's failure (a spawn), shown until the next terminal opens.
    pub notice: Option<String>,
    /// The command palette, while open, and its field and highlighted row.
    pub palette_open: bool,
    pub palette_query: rune::TextEdit,
    pub palette_picked: usize,
    pub sidebar_open: bool,
    /// Find in the focused pane's history, while open.
    pub find: Option<find::Find>,
    pub find_query: rune::TextEdit,
    /// The link under the pointer while ⌘ is held.
    pub pointer: links::Pointer,
    /// Settings that outlive the window: text size, the sidebar.
    prefs: rune::prefs::Prefs,
    /// What every terminal is doing, for the sidebar's marks.
    pub activities: crate::activity::Activities,
    /// The input method's composition at the terminal cursor.
    pub preedit: Option<String>,
    /// The blink phase a blinking cursor shows now, and when typing last
    /// restarted it (app clock seconds).
    pub blink_on: bool,
    blink_since: f64,
    clock: f64,
    /// Spawns this window sent, to show the terminal when it lands.
    pending_spawns: Vec<u32>,
    /// Bumped by every frame change of the shown terminal, so the view runs.
    pub generation: u64,
    pub font_size: f32,
    /// The window's device pixels per point, as the last view build saw it.
    pub scale: Cell<f32>,
    /// Each pane's grid area in points, from its last layout.
    areas: HashMap<ResourceId, [f32; 2]>,
    /// The size this window last asked the server for, per terminal.
    fitted: HashMap<ResourceId, Fitted>,
    /// What the terminal area shows: the session's split tree, or the
    /// focused pane alone.
    pub layout: Option<LayoutNode>,
    /// Views of the panes on screen besides the focused one.
    pub others: HashMap<ResourceId, ViewId>,
    /// The pane shown alone, by this window's choice.
    pub zoomed: Option<ResourceId>,
    /// A divider drag's split and ratio, previewed until release.
    pub(crate) dragging: Option<(SplitPath, f32)>,
    /// Spawned splits waiting for the session's arrangement: the pane split,
    /// the new terminal, the direction, and since when.
    unplaced_splits: Vec<(ResourceId, ResourceId, SplitDir, Instant)>,
    /// Arrangement writes this window made, to report a refusal.
    layout_ops: HashSet<OperationId>,
    /// Spawns that become splits: the pane split and the direction.
    pending_splits: HashMap<u32, (ResourceId, SplitDir)>,
    /// The pane the wheel last moved, owning `wheel_rows`.
    wheel_pane: Option<ResourceId>,
    /// The pane whose program took the last press, until its release.
    mouse_capture: Option<ResourceId>,
    /// The cell the last reported pointer event was in.
    mouse_cell: Option<(u16, u16)>,
    /// The window title last set.
    window_title: String,
    /// Each split's size in points, for turning a divider drag into a ratio.
    split_sizes: HashMap<SplitPath, [f32; 2]>,
    /// Arrangements already asked for, so a missing one is fetched once.
    fetched: HashSet<ArrangementKey>,
    pub focused: bool,
    gesture: u64,
    wheel_rows: f32,
    sessions_seq: u32,
    /// The fenced frame the last view build painted, for the drawn receipt.
    pub painted: RefCell<Option<Painted>>,
}

impl Desk {
    pub fn new(resolved: &Resolved) -> Self {
        // `RUNE_PREFS_DIR` keeps headless runs off the real settings.
        let prefs = rune::prefs::Prefs::open(PREFS_ID);
        let font_size = prefs
            .get::<f32>("font_size")
            .map_or(grid::DEFAULT_FONT_SIZE, |s| {
                s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
            });
        let sidebar_open = prefs.get("sidebar_open").unwrap_or(true);
        let mut desk = Self {
            client: None,
            error: None,
            status: Status::Idle,
            topology: None,
            server: None,
            shown: None,
            wanted: None,
            pending_session: None,
            epoch: 0,
            pending: None,
            notice: None,
            palette_open: false,
            palette_query: rune::TextEdit::new(""),
            palette_picked: 0,
            sidebar_open,
            find: None,
            find_query: rune::TextEdit::new(""),
            pointer: links::Pointer::default(),
            prefs,
            preedit: None,
            activities: crate::activity::Activities::default(),
            blink_on: true,
            blink_since: 0.0,
            clock: 0.0,
            pending_spawns: Vec::new(),
            generation: 0,
            font_size,
            scale: Cell::new(2.0),
            areas: HashMap::new(),
            fitted: HashMap::new(),
            layout: None,
            others: HashMap::new(),
            zoomed: None,
            dragging: None,
            pending_splits: HashMap::new(),
            unplaced_splits: Vec::new(),
            layout_ops: HashSet::new(),
            wheel_pane: None,
            mouse_capture: None,
            mouse_cell: None,
            window_title: String::new(),
            split_sizes: HashMap::new(),
            fetched: HashSet::new(),
            focused: true,
            gesture: 0,
            wheel_rows: 0.0,
            sessions_seq: 0,
            painted: RefCell::new(None),
        };
        match &resolved.socket {
            Ok(socket) => match sessions::connect(socket, resolved.session.clone()) {
                Ok(client) => desk.client = Some(Arc::new(client)),
                Err(error) => desk.error = Some(error),
            },
            Err(error) => desk.error = Some(error.clone()),
        }
        desk
    }

    pub fn metrics(&self) -> Metrics {
        Metrics::new(self.font_size, self.scale.get())
    }

    /// Rune's setup hook: route runtime wakes into Rune's loop.
    pub fn listen(&mut self, waker: Waker) {
        if let Some(client) = &self.client {
            client.set_listener(Arc::new(Wake(waker)));
        }
    }

    /// Drain the runtime once. True when anything on screen may have changed.
    pub fn poll(&mut self, clock: f64, tx: &mut Tx) -> bool {
        self.clock = clock;
        let Some(client) = self.client.clone() else {
            return false;
        };
        let observation = client.take_activity();
        let mut changed = observation.status != self.status;
        if observation.connection_epoch != self.epoch {
            self.epoch = observation.connection_epoch;
            changed |= self.reopen_after_reconnect(&client);
        }
        self.status = observation.status;
        if let Some(error) = observation.last_error.filter(|_| self.status.is_terminal()) {
            changed |= self.error.as_ref() != Some(&error);
            self.error = Some(error);
        }
        if let Some(topology) = client.topology()
            && self.topology.as_ref() != Some(&topology)
        {
            self.topology = Some(topology);
            changed = true;
        }
        if self.server.is_none() {
            self.server = client.server().map(|info| format!("{info:?}"));
        }
        let events = !observation.events.is_empty();
        for event in observation.events {
            changed |= self.apply(&client, event, tx);
        }
        changed |= self.open_wanted(&client);
        self.title(tx);
        if changed || events {
            changed |= self.refresh_layout(&client);
        }
        self.keep_find();
        // Also catches a display-scale change, which re-snaps the cells.
        self.fit();
        changed | self.blink(&client, tx)
    }

    fn apply(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
        let on_screen: Vec<ResourceId> = match &self.layout {
            Some(tree) => phux_client_core::layout::leaves(tree),
            None => self.shown.iter().map(|s| s.terminal.clone()).collect(),
        };
        let marked = self.activities.apply(&event, &on_screen);
        marked | self.apply_view(client, event, tx)
    }

    fn apply_view(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
        let shown = |id: &ResourceId| self.shown.as_ref().is_some_and(|s| &s.terminal == id);
        match event {
            Event::TerminalChanged { terminal_id }
                if shown(&terminal_id) || self.others.contains_key(&terminal_id) =>
            {
                self.relink(&terminal_id);
                self.generation += 1;
                true
            }
            Event::TerminalSpawned {
                request_id,
                terminal_id: Some(new),
                ..
            } if self.pending_splits.contains_key(&request_id) => {
                if let Some((target, dir)) = self.pending_splits.remove(&request_id) {
                    self.place_split(client, &target, &new, dir);
                }
                true
            }
            Event::TerminalSpawned {
                request_id,
                terminal_id,
                error,
            } if self.pending_spawns.contains(&request_id) => {
                self.pending_spawns.retain(|id| *id != request_id);
                match (terminal_id, error) {
                    (Some(id), _) => self.want(id),
                    (None, error) => {
                        self.notice = error.or_else(|| Some("spawn failed".to_owned()))
                    }
                }
                true
            }
            Event::TerminalClosed { terminal_id, .. } if shown(&terminal_id) => {
                // Keep the keyboard in the split: the neighbour takes focus.
                let next = self
                    .layout
                    .as_ref()
                    .and_then(|t| layout::neighbour(t, &terminal_id));
                self.close_shown(client);
                if let Some(next) = next {
                    self.focus_pane(&next);
                }
                true
            }
            Event::OrganizationOperation { operation, outcome }
                if self.layout_ops.remove(&operation) =>
            {
                if matches!(outcome, OrgOutcome::Refused(_) | OrgOutcome::Error { .. }) {
                    self.notice = Some("The layout changed elsewhere first; try again.".to_owned());
                }
                true
            }
            Event::TerminalClosed { terminal_id, .. } if self.others.contains_key(&terminal_id) => {
                if let Some(view) = self.others.remove(&terminal_id) {
                    let _ = client.destroy_view(view);
                }
                true
            }
            // OSC 52 only from the terminal on screen, while the window has
            // focus; never retained for later.
            Event::ClipboardWrite { terminal_id, text } if shown(&terminal_id) && self.focused => {
                tx.copy(text.0);
                false
            }
            Event::StatusChanged(_)
            | Event::TopologyChanged
            | Event::OrganizationChanged
            | Event::Attached { .. } => true,
            _ => false,
        }
    }

    fn want(&mut self, terminal: ResourceId) {
        self.wanted = Some((terminal, Instant::now()));
    }

    fn drop_shown(&mut self, client: &Client) -> Option<ResourceId> {
        let shown = self.shown.take()?;
        let _ = client.destroy_view(shown.view);
        self.fitted.remove(&shown.terminal);
        self.painted.replace(None);
        Some(shown.terminal)
    }

    fn close_shown(&mut self, client: &Client) {
        self.drop_shown(client);
        // Fall back to whatever the server now focuses.
        self.wanted = None;
    }

    /// A new connection may be a new server incarnation, whose engine forgot
    /// every view: open the shown terminal again on this connection.
    fn reopen_after_reconnect(&mut self, client: &Client) -> bool {
        for (_, view) in self.others.drain() {
            let _ = client.destroy_view(view);
        }
        self.fetched.clear();
        self.fitted.clear();
        let Some(terminal) = self.drop_shown(client) else {
            return false;
        };
        if self.wanted.is_none() {
            self.want(terminal);
        }
        true
    }

    /// Which terminal should be on screen, if it is not yet: the one the user
    /// asked for, a new session's once it is selected, else the server's
    /// focus. `None` also while waiting on one of those.
    fn target(&mut self, client: &Client) -> Option<ResourceId> {
        let topology = self.topology.as_ref()?;
        if let Some((terminal, since)) = &self.wanted {
            let listed = topology.panes.iter().any(|p| &p.terminal_id == terminal);
            if listed || since.elapsed() < WANTED_GRACE {
                return Some(terminal.clone());
            }
            // Gone (a restarted server, a closed pane): fall back below.
            self.wanted = None;
        }
        if let Some(name) = &self.pending_session {
            let session = topology.sessions.iter().find(|s| &s.name == name)?;
            if client.selected_session() != Some(session.id) {
                return None;
            }
            let panes = || topology.panes.iter().filter(|p| p.session_id == session.id);
            let pane = panes()
                .find(|p| p.terminal_id == topology.focused_pane)
                .or_else(|| panes().next())?;
            let terminal = pane.terminal_id.clone();
            self.pending_session = None;
            return Some(terminal);
        }
        if self.shown.is_some() {
            return None;
        }
        sessions::default_pane(topology)
    }

    /// Open the target terminal once its replica is ready. Retried on every
    /// drain until it succeeds; the runtime streams attached terminals on
    /// its own, so nothing here forces a reconnect.
    fn open_wanted(&mut self, client: &Client) -> bool {
        let Some(target) = self.target(client) else {
            return false;
        };
        if self.shown.as_ref().is_some_and(|s| s.terminal == target) {
            self.wanted = None;
            return false;
        }
        if self.others.contains_key(&target) {
            self.focus_pane(&target);
            self.wanted = None;
            return true;
        }
        let Ok(view) = client.create_view(&target) else {
            self.want(target);
            return false;
        };
        // The old focus stays on screen if the layout still shows it.
        if let Some(old) = self.shown.take() {
            self.others.insert(old.terminal, old.view);
            self.painted.replace(None);
        }
        self.activities.seen(&target);
        self.shown = Some(Shown {
            terminal: target,
            view,
        });
        self.wanted = None;
        self.notice = None;
        self.generation += 1;
        true
    }

    pub fn zoom(&mut self, size: Option<f32>) {
        self.font_size = size.map_or(grid::DEFAULT_FONT_SIZE, |s| {
            s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
        });
        let _ = self.prefs.set("font_size", &self.font_size);
        self.fitted.clear();
        self.fit();
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
        let _ = self.prefs.set("sidebar_open", &self.sidebar_open);
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        let Some(client) = &self.client else { return };
        if let Some(shown) = self.shown.take() {
            let _ = client.destroy_view(shown.view);
        }
        for (_, view) in self.others.drain() {
            let _ = client.destroy_view(view);
        }
    }
}
