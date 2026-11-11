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
use phux_client_runtime::control::{Observation, ProjectionFence, Status, Topology};
use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client, Listener, ViewId};
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceId, SessionKey};
use rune::{Tx, Waker};

use crate::grid::{self, Metrics};
use crate::launch::Resolved;

mod attach;
pub(crate) mod bare;
mod copy_mode;
mod events;
mod find;
mod fleet;
mod frame;
pub mod galaxy;
pub mod hive;
mod home;
mod inbox;
mod input;
pub mod kitchen;
mod layout;
pub mod link;
mod links;
pub mod machines;
pub mod mouse;
pub(crate) mod native_input;
pub mod news;
mod notice;
pub mod onboarding;
mod palette;
pub mod phone;
pub mod pitch;
pub mod pitlane;
pub mod questions;
pub mod reef;
mod reorder;
pub mod resources;
mod sessions;
mod settings;
mod shown;
mod splits;
mod tabs;
pub mod updates;
mod worktree;

pub use layout::SplitPath;
pub use notice::{Notice, NoticeKind};
pub use palette::{CONTEXT_MENU, with_menus};
pub use reorder::Grab;
pub use settings::Category as SettingsCategory;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Explicit consent to start a supervised read-only mirror producer.
    Mirror(resources::Source, String),
    /// A line-breaking paste, and the terminal it was meant for.
    Paste(ResourceId, String),
    /// Closing a terminal that is running a command.
    Close(ResourceId),
    /// Closing every terminal of a tab.
    CloseTab(Vec<ResourceId>),
    /// Ending a session and everything in it.
    CloseSession(SessionKey, String),
}

/// Where a spawned terminal goes in the arrangement.
type Placement = Option<(ResourceId, SplitDir)>;

/// Focused resource identity, with a view only for a terminal replica.
pub struct Shown {
    pub resource: ResourceId,
    pub view: Option<ViewId>,
}

pub struct Desk {
    pub client: Option<Arc<Client>>,
    /// Why there is no client, or the last failure the user should see.
    pub error: Option<String>,
    pub status: Status,
    pub topology: Option<Topology>,
    pub server: Option<String>,
    pub shown: Option<Shown>,
    pub resources: resources::Resources,
    /// The terminal the user asked for, and since when, until its view opens.
    pub wanted: Option<(ResourceId, Instant)>,
    /// A session this window created: show its terminal once it is selected.
    pending_session: Option<String>,
    /// The connection incarnation the shown view belongs to.
    epoch: u64,
    /// An action waiting for the user's confirmation.
    pub pending: Option<Pending>,
    /// What the user was last told, until it expires or they dismiss it. A
    /// terminal opening takes down only what was in passing.
    pub notice: Option<Notice>,
    /// The command palette, while open, and its field and highlighted row.
    pub palette_open: bool,
    pub palette_query: rune::TextEdit,
    pub palette_picked: usize,
    /// What the palette lists.
    pub palette_mode: crate::palette::Mode,
    /// The theme a live preview stands in for, restored unless one is chosen.
    theme_before: Option<Option<&'static crate::theme::Theme>>,
    /// The path search the palette shows, while it lists paths.
    pub paths: Option<palette::PathSearch>,
    /// Each pane's grid origin in window points, from its last layout.
    origins: HashMap<ResourceId, [f32; 2]>,
    pub sidebar_open: bool,
    /// The window's width in points, from the root's last layout; 0 until then.
    pub window_w: f32,
    /// The sidebar shown over the preference in a window too narrow for it.
    pub sidebar_peek: bool,
    /// The sidebar's foot button under the pointer, by key, so it can show
    /// its chord.
    pub foot_hover: Option<&'static str>,
    /// Find in the focused pane's history, while open.
    pub find: Option<find::Find>,
    pub find_query: rune::TextEdit,
    /// Keyboard copy mode, while on.
    pub copy_mode: Option<copy_mode::CopyMode>,
    /// The link under the pointer while ⌘ is held.
    pub pointer: links::Pointer,
    /// Everything waiting on you, and the system's view of it.
    pub inbox: inbox::Inbox,
    /// The fleet as a galaxy, while open.
    pub galaxy: galaxy::Galaxy,
    /// The fleet as a reef, while open.
    pub reef: reef::Reef,
    /// The fleet as football training, while open.
    pub pitch: pitch::Pitch,
    /// The fleet as a kitchen, while open.
    pub kitchen: kitchen::Kitchen,
    /// The fleet as a pit lane, while open.
    pub pitlane: pitlane::PitLane,
    /// The fleet as beehives, while open.
    pub hive: hive::Hive,
    /// The other machines, watched while the galaxy or the reef is open.
    pub(crate) fleet: fleet::Fleet,
    pub settings: settings::Settings,
    /// `config.toml`'s `[rune]` keys, which win over saved choices.
    pub file: phux_config_core::RuneCfg,
    pub settings_open: bool,
    /// First-run onboarding, while open, and what it has learned.
    pub onboarding: onboarding::Onboarding,
    /// What's new, and whether its bell is lit.
    pub news: news::News,
    /// Navigation and search belong to this window, never to stored preferences.
    pub settings_ui: settings::SettingsUi,
    pub tabs: tabs::Tabs,
    /// A tab or session drag, and the session order the server keeps.
    pub reorder: reorder::Reorder,
    pub updater: updates::Updater,
    pub machines: machines::Machines,
    pub link: link::Link,
    /// How this run started, for moving between machines.
    launch: Resolved,
    waker: Option<Waker>,
    /// Image uploads on their way, and the terminal each is for.
    uploads: Vec<(u64, ResourceId)>,
    /// User pastes the server hasn't answered yet.
    pastes: HashSet<u64>,
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
    receipts: events::Receipts,
    /// Bumped by every frame change of the shown terminal, so the view runs.
    pub generation: u64,
    pub font_size: f32,
    /// The terminal font family; empty is the default.
    pub font_family: String,
    /// `font_family` is not behind Rune's mono slot yet.
    font_due: bool,
    /// `rune::text::font_epoch` when the panes were last fitted.
    font_epoch: u64,
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
    pub others: HashMap<ResourceId, Option<ViewId>>,
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
    /// The pane whose program took the last press, and that press as it last
    /// moved, until its release.
    mouse_capture: Option<(ResourceId, mouse::Press)>,
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
    /// The terminal last told it has the window's focus.
    focus_told: Option<ResourceId>,
    /// The arrangement write in flight for a split or tab. One at a time:
    /// each write's base is the document the last one made.
    split_write: Option<splits::SplitWrite>,
    /// The phone form's state, when this window is one.
    pub phone: Option<phone::Phone>,
    /// The bare window: one pane, the terminal UI in it, no chrome.
    pub bare: Option<bare::Bare>,
    pub native: Option<crate::mobile::Platform>,
}

impl Desk {
    pub fn new(resolved: &Resolved) -> Self {
        let remembered = (!resolved.aimed)
            .then(|| rune::prefs::Prefs::open(PREFS_ID).get::<String>("machine"))
            .flatten()
            .filter(|machine| !machine.is_empty());
        Self::open(resolved, remembered)
    }

    /// A window with no connection yet; `dial` makes one. `machine` is a
    /// registered remote; `None` is this computer's server.
    fn open(resolved: &Resolved, machine: Option<String>) -> Self {
        // `RUNE_PREFS_DIR` keeps headless runs off the real settings.
        let prefs = rune::prefs::Prefs::open(PREFS_ID);
        let (file, file_error) = settings::load_file();
        let font_size = file
            .font_size
            .or_else(|| prefs.get::<f32>("font_size"))
            .map_or(grid::DEFAULT_FONT_SIZE, |s| {
                s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
            });
        let font_family: String = (file.font_family.clone())
            .or_else(|| prefs.get("font_family"))
            .unwrap_or_default();
        let sidebar_open = prefs.get("sidebar_open").unwrap_or(true);
        let settings = settings::Settings::load(&prefs).over(&file);
        let mut desk = Self {
            client: None,
            error: None,
            status: Status::Idle,
            topology: None,
            server: None,
            shown: None,
            resources: resources::Resources::default(),
            wanted: None,
            pending_session: None,
            epoch: 0,
            pending: None,
            notice: None,
            palette_open: false,
            palette_query: rune::TextEdit::new(""),
            palette_picked: 0,
            palette_mode: crate::palette::Mode::All,
            theme_before: None,
            paths: None,
            origins: HashMap::new(),
            sidebar_open,
            window_w: 0.0,
            sidebar_peek: false,
            foot_hover: None,
            find: None,
            copy_mode: None,
            find_query: rune::TextEdit::new(""),
            pointer: links::Pointer::default(),
            inbox: inbox::Inbox::default(),
            galaxy: galaxy::Galaxy::default(),
            reef: reef::Reef::default(),
            pitch: pitch::Pitch::default(),
            kitchen: kitchen::Kitchen::default(),
            pitlane: pitlane::PitLane::default(),
            hive: hive::Hive::default(),
            fleet: fleet::Fleet::default(),
            settings,
            settings_open: false,
            onboarding: onboarding::Onboarding::default(),
            news: news::News::load(&prefs),
            settings_ui: settings::SettingsUi::default(),
            tabs: tabs::Tabs::default(),
            reorder: reorder::Reorder::default(),
            updater: updates::Updater::new(&prefs, resolved),
            machines: machines::Machines::new(machine, resolved.phux.clone()),
            link: link::Link::default(),
            launch: resolved.clone(),
            waker: None,
            uploads: Vec::new(),
            pastes: HashSet::new(),
            prefs,
            preedit: None,
            activities: crate::activity::Activities::default(),
            blink_on: true,
            blink_since: 0.0,
            clock: 0.0,
            pending_spawns: Vec::new(),
            receipts: events::Receipts::default(),
            generation: 0,
            font_size,
            font_due: !font_family.is_empty(),
            font_family,
            font_epoch: rune::text::font_epoch(),
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
            focus_told: None,
            split_write: None,
            phone: None,
            bare: None,
            native: None,
            file,
        };
        if let Some(error) = file_error {
            desk.say(NoticeKind::Error, error);
        }
        desk
    }

    pub fn metrics(&self) -> Metrics {
        Metrics::new(self.font_size, self.scale.get())
    }

    /// Drain the runtime once. True when anything on screen may have changed.
    pub fn poll(&mut self, clock: f64, tx: &mut Tx) -> bool {
        self.clock = clock;
        self.follow_font(tx);
        self.machines.tick_glances(clock, tx);
        self.follow_path(tx);
        let updated = self.pump_updates(tx)
            | self.finish_document_images()
            | self.pump_machines()
            | self.tick_fleet()
            | self.pump_link()
            | self.pump_links()
            | self.follow_omarchy()
            | self.pump_onboarding(tx)
            | self.expire_notice(tx);
        let Some(client) = self.client.clone() else {
            return updated | self.tend_link(tx);
        };
        let (news, moved) = self.observe(&client, client.take_activity(), tx);
        let mut changed = updated | news | self.take_globals(&client);
        let rearrange = updated || moved;
        changed |= self.drop_dead_views(&client);
        let opened = self.settle_home(&client) | self.open_wanted(&client);
        changed |= opened;
        let unfocused = self.sync_focus(&client);
        self.title(tx);
        if rearrange || opened || self.layout_is_stale() {
            changed |= self.refresh_layout(&client);
        }
        // The replica's news may never come, and a split only waits so long.
        if unfocused || self.splits_waiting() {
            tx.wake_after(Duration::from_millis(250));
        }
        changed |= self.finish_uploads(&client);
        changed |= self.take_paths(&client) | self.take_screens();
        changed |= self.sync_alerts();
        changed |= self.tick_inbox(tx);
        changed |= self.tick_galaxy(tx);
        changed |= self.tick_reef(tx);
        changed |= self.tick_pitch(tx);
        changed |= self.tick_kitchen(tx);
        changed |= self.tick_pitlane(tx);
        changed |= self.tick_hive(tx);
        self.settle_pointer();
        changed |= self.autoscroll(tx);
        changed |= self.phone_touch_tick(tx);
        self.keep_find();
        self.keep_copy_mode();
        // Also re-snaps cells after a display-scale change.
        self.fit();
        self.wake_phone_fit(tx);
        changed | self.blink(&client, tx) | self.tend_link(tx)
    }

    /// What an observation says about the connection itself. The topology
    /// is only sampled when the batch can have changed it.
    fn follow_connection(&mut self, client: &Client, observation: &mut Observation) -> bool {
        let mut changed = observation.status != self.status;
        if observation.connection_epoch != self.epoch {
            self.retire_documents(client);
            self.first_contact();
            self.epoch = observation.connection_epoch;
            self.focus_told = None;
            changed |= self.reopen_after_reconnect(client);
        }
        self.status = observation.status;
        let failure = observation.last_error.take();
        if let Some(error) = failure.filter(|_| self.status.is_terminal()) {
            changed |= self.error.as_ref() != Some(&error);
            self.failed_with(&error);
            self.error = Some(error);
        }
        if let Some(topology) = observation.topology.take()
            && self.topology.as_ref() != Some(&topology)
        {
            self.topology = Some(topology);
            changed = true;
        }
        if self.server.is_none() {
            self.server = client.server().map(|info| format!("{info:?}"));
        }
        if changed || observation.events.iter().any(events::changes_resources) {
            changed |= self.refresh_resources(client);
        }
        changed
    }
}
