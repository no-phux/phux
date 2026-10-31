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
use phux_client_runtime::{Client, Listener, Target, ViewId};
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceId, SessionKey};
use rune::{Tx, Waker};

use crate::grid::{self, Metrics};
use crate::launch::Resolved;

mod attach;
mod find;
mod frame;
mod inbox;
mod input;
mod layout;
mod links;
pub mod machines;
mod mouse;
mod palette;
mod sessions;
mod settings;
mod shown;
mod tabs;
pub mod updates;

pub use layout::SplitPath;
pub use tabs::{Renaming, Tab};

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
/// What kind of thing a notice says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Error,
}

impl NoticeKind {
    /// Seconds on screen; a failure stays long enough to be read twice.
    const fn lasts(self) -> f64 {
        match self {
            Self::Info => 6.0,
            Self::Error => 12.0,
        }
    }
}

/// Words for the user, in a toast and the inbox's footer.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    /// When it goes, on the app clock.
    expires: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// A paste that would type line breaks into a program.
    Paste(String),
    /// Closing a terminal that is running a command.
    Close(ResourceId),
    /// Closing every terminal of a tab.
    CloseTab(Vec<ResourceId>),
    /// Ending a session and everything in it.
    CloseSession(SessionKey, String),
}

/// Where a spawned terminal goes in the arrangement.
type Placement = Option<(ResourceId, SplitDir)>;

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
    /// What the user was last told, until it expires, they dismiss it, or
    /// the next terminal opens.
    pub notice: Option<Notice>,
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
    /// Everything waiting on you, and the system's view of it.
    pub inbox: inbox::Inbox,
    pub settings: settings::Settings,
    pub settings_open: bool,
    pub tabs: tabs::Tabs,
    pub updater: updates::Updater,
    pub machines: machines::Machines,
    /// How this run started, for moving between machines.
    launch: Resolved,
    waker: Option<Waker>,
    /// Image uploads on their way, and the terminal each is for.
    uploads: Vec<(u64, ResourceId)>,
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
    /// Spawned splits and tabs waiting for the session's arrangement: the
    /// new terminal, where it goes, and since when.
    unplaced_splits: Vec<(ResourceId, Placement, Instant)>,
    /// Arrangement writes this window made, to report a refusal.
    layout_ops: HashSet<OperationId>,
    /// Spawns that become splits (the pane split and the direction) or, with
    /// `None`, tabs.
    pending_splits: HashMap<u32, Placement>,
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
    /// Put a line in front of the user.
    pub fn say(&mut self, kind: NoticeKind, text: impl Into<String>) {
        let text = text.into();
        let expires = self.clock + kind.lasts();
        self.notice = Some(Notice {
            kind,
            text,
            expires,
        });
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// Take down a notice whose time is up, and wake for the one still
    /// showing. "Uploading" stays for as long as that's true.
    fn expire_notice(&mut self, tx: &mut Tx) -> bool {
        let Some(notice) = &self.notice else {
            return false;
        };
        if notice.kind == NoticeKind::Info && !self.uploads.is_empty() {
            return false;
        }
        let left = notice.expires - self.clock;
        if left > 0.0 {
            tx.wake_after(Duration::from_secs_f64(left));
            return false;
        }
        self.notice = None;
        true
    }

    pub fn new(resolved: &Resolved) -> Self {
        let remembered = (!resolved.aimed)
            .then(|| rune::prefs::Prefs::open(PREFS_ID).get::<String>("machine"))
            .flatten()
            .filter(|machine| !machine.is_empty());
        Self::open(resolved, remembered)
    }

    /// `machine` is a registered remote; `None` is this computer's server.
    fn open(resolved: &Resolved, machine: Option<String>) -> Self {
        // `RUNE_PREFS_DIR` keeps headless runs off the real settings.
        let prefs = rune::prefs::Prefs::open(PREFS_ID);
        let font_size = prefs
            .get::<f32>("font_size")
            .map_or(grid::DEFAULT_FONT_SIZE, |s| {
                s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
            });
        let sidebar_open = prefs.get("sidebar_open").unwrap_or(true);
        let settings = settings::Settings::load(&prefs);
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
            inbox: inbox::Inbox::default(),
            settings,
            settings_open: false,
            tabs: tabs::Tabs::default(),
            updater: updates::Updater::new(&prefs, resolved),
            machines: machines::Machines::new(machine.clone(), resolved.phux.clone()),
            launch: resolved.clone(),
            waker: None,
            uploads: Vec::new(),
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
        let target = match &machine {
            Some(machine) => Target::resolve(machine, None),
            None => resolved.socket.clone().map(Target::uds),
        };
        match target.and_then(|target| sessions::connect(target, resolved.session.clone())) {
            Ok(client) => desk.client = Some(Arc::new(client)),
            Err(error) => desk.error = Some(error),
        }
        desk.apply_theme();
        desk
    }

    pub fn metrics(&self) -> Metrics {
        Metrics::new(self.font_size, self.scale.get())
    }

    /// Rune's setup hook: route runtime wakes into Rune's loop.
    pub fn listen(&mut self, waker: Waker) {
        self.updater.start(waker.clone());
        self.machines.listen(waker.clone());
        self.watch(waker);
    }

    /// Route this connection's wakes and alerts into Rune's loop.
    fn watch(&mut self, waker: Waker) {
        self.waker = Some(waker.clone());
        self.inbox.alerts.listen(waker.clone());
        if let Some(client) = &self.client {
            client.set_listener(Arc::new(Wake(waker)));
            client.watch_approvals();
        }
    }

    /// Drain the runtime once. True when anything on screen may have changed.
    pub fn poll(&mut self, clock: f64, tx: &mut Tx) -> bool {
        self.clock = clock;
        let updated = self.pump_updates(tx) | self.pump_machines() | self.expire_notice(tx);
        let Some(client) = self.client.clone() else {
            return updated;
        };
        let observation = client.take_activity();
        let mut changed = updated | (observation.status != self.status);
        if observation.connection_epoch != self.epoch {
            // Only the first connection replays news: a reconnect's items
            // are known by id, and what arrived during the gap is new.
            if self.epoch == 0 {
                self.inbox.alerts.connected();
            }
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
        if changed || events || self.layout_is_stale() {
            changed |= self.refresh_layout(&client);
        }
        self.finish_uploads(&client);
        changed |= self.sync_alerts();
        self.keep_find();
        // Also catches a display-scale change, which re-snaps the cells.
        self.fit();
        changed | self.blink(&client, tx)
    }

    fn apply(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
        // A background window shows nothing to anyone.
        let on_screen = if self.focused {
            self.on_screen()
        } else {
            Vec::new()
        };
        let marked = self.activities.apply(&event, &on_screen);
        let inbox = self.inbox_event(client, &event);
        marked | inbox | self.apply_view(client, event, tx)
    }

    /// The terminals in front of the person.
    pub(super) fn on_screen(&self) -> Vec<ResourceId> {
        match &self.layout {
            Some(tree) => phux_client_core::layout::leaves(tree),
            None => self.shown.iter().map(|s| s.terminal.clone()).collect(),
        }
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
                if let Some(placement) = self.pending_splits.remove(&request_id) {
                    self.place_split(client, &new, placement);
                }
                true
            }
            Event::TerminalSpawned {
                request_id,
                terminal_id,
                error,
            } if self.pending_spawns.contains(&request_id)
                || self.pending_splits.contains_key(&request_id) =>
            {
                self.pending_spawns.retain(|id| *id != request_id);
                self.pending_splits.remove(&request_id);
                match (terminal_id, error) {
                    (Some(id), _) => self.want(id),
                    (None, error) => self.say(
                        NoticeKind::Error,
                        error.unwrap_or_else(|| "spawn failed".to_owned()),
                    ),
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
                    self.say(
                        NoticeKind::Error,
                        "The layout changed elsewhere first; try again.",
                    );
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
