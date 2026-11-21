//! The window's composition root and its persistent UI/platform state.
//!
//! The runtime does the terminal (replica, reconnect, acknowledged input,
//! history, selection). `connection` owns one client's workspace and request
//! namespace; replacing it leaves window preferences and platform services alone.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use phux_client_core::layout::SplitDir;
use phux_client_runtime::ViewId;
use phux_client_runtime::control::ProjectionFence;
use phux_client_runtime::publication::GridFrame;
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Tx, Waker};

use crate::grid::{self, Metrics};
use crate::launch::Resolved;

pub mod accessibility;
mod attach;
pub(crate) mod bare;
mod changes;
mod clipboard_read;
mod clipboard_write;
mod compat;
pub mod connection;
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
pub mod notice;
pub mod onboarding;
mod palette;
mod pending;
mod picks;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod plugin_actions;
pub use pending::Pending;
pub mod phone;
pub mod pitch;
pub mod pitlane;
pub mod popup;
pub mod questions;
pub mod quick;
mod quick_look;
pub mod reef;
mod reorder;
pub mod resources;
mod reveal;
mod scene_host;
mod screenshot;
mod sessions;
mod settings;
mod shown;
mod splits;
pub mod surfaces;
mod tabs;
pub mod updates;
pub(crate) mod window_frame;
mod worker;
mod worktree;

pub use layout::SplitPath;
pub use notice::{Notice, NoticeKind};
pub use palette::{CONTEXT_MENU, TAB_MENU, with_menus};
pub use reorder::Grab;
pub use settings::Category as SettingsCategory;
pub use settings::INTERFACE_SIZES;
pub use tabs::{Renaming, Tab};

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

/// Where a spawned terminal goes in the arrangement.
type Placement = Option<(ResourceId, SplitDir)>;

/// Focused resource identity, with a view only for a terminal replica.
pub struct Shown {
    pub resource: ResourceId,
    pub view: Option<ViewId>,
}

pub struct Desk {
    pub connection: connection::Connection,
    /// An action waiting for the user's confirmation.
    pub pending: Option<Pending>,
    /// The front toast: what the user was last told, until it expires or
    /// they dismiss it. A terminal opening takes down only what was in
    /// passing. A copy of `toasts`' front for one-line readers; change it
    /// through `say`, `dismiss_notice` and the rest, never directly.
    pub notice: Option<Notice>,
    /// Every toast, newest in front, and the kit stack's state.
    toasts: notice::Toasts,
    /// Typed input the shown terminal turned away, for qualification evidence.
    pub(crate) input_refusals: u64,
    /// State the rune-kit components keep between runs: the open menu and
    /// select.
    /// The system's reduce-transparency and text-size reads.
    pub(crate) accessibility: accessibility::Accessibility,
    /// The command palette, while open, and its field and highlighted row.
    pub palette_open: bool,
    pub palette_query: rune::TextEdit,
    pub palette_picked: usize,
    /// What the palette lists.
    pub palette_mode: crate::palette::Mode,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    plugin_actions: plugin_actions::Actions,
    /// The theme a live preview stands in for, restored unless one is chosen.
    theme_before: Option<Option<&'static crate::theme::Theme>>,
    pub sidebar_open: bool,
    /// The window's width in points, from the root's last layout; 0 until then.
    pub window_w: f32,
    window_h: f32,
    /// The sidebar shown over the preference in a window too narrow for it.
    pub sidebar_peek: bool,
    /// `display-panes`: the numbered panes, until the next key.
    pub pane_numbers: Vec<ResourceId>,
    /// The tab whose terminals all get what is typed (`sync-panes`).
    pub sync_tab: Option<[u8; 16]>,
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
    /// Mutually exclusive fleet scene; scene state itself may be retained.
    pub playground: Option<surfaces::Playground>,
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
    /// Session disclosure belongs to this window, independent of selection.
    pub sidebar_collapsed: HashSet<SessionKey>,
    pub updater: updates::Updater,
    pub machines: machines::Machines,
    pub link: link::Link,
    /// How this run started, for moving between machines.
    launch: Resolved,
    waker: Option<Waker>,
    /// Settings that outlive the window: text size, the sidebar.
    prefs: rune::prefs::Prefs,
    /// Directory of the shared first-run step. Tests leave this empty until
    /// they pass a temp directory; production is the server state directory.
    onboarding_dir: Option<PathBuf>,
    /// What every terminal is doing, for the sidebar's marks.
    pub activities: crate::activity::Activities,
    /// The blink phase a blinking cursor shows now, and when typing last
    /// restarted it (app clock seconds).
    pub blink_on: bool,
    blink_since: f64,
    /// The shown terminal's grid size as last seen, until when (app clock)
    /// the title bar shows it after a window resize, and whether it did.
    size_seen: Option<(u16, u16)>,
    size_until: f64,
    size_was_shown: bool,
    clock: f64,
    /// Bumped by every frame change of the shown terminal, so the view runs.
    pub generation: u64,
    pub font_size: f32,
    /// The chrome's size in percent (`settings::INTERFACE_SIZES`): the
    /// view's zoom, which the terminal's text is held against.
    pub interface_size: u16,
    /// The terminal font family; empty is the default.
    pub font_family: String,
    /// `font_family` is not behind Rune's mono slot yet.
    font_due: bool,
    /// `rune::text::font_epoch` when the panes were last fitted.
    font_epoch: u64,
    /// The window's device pixels per point, as the last view build saw it.
    pub scale: Cell<f32>,
    /// The system appearance as the last view build saw it; `None` before.
    pub system_dark: Cell<Option<bool>>,
    /// The window title last set.
    window_title: String,
    pub focused: bool,
    /// The phone form's state, when this window is one.
    pub phone: Option<phone::Phone>,
    /// The bare window: one pane, the terminal UI in it, no chrome.
    pub bare: Option<bare::Bare>,
    /// The quick terminal's own part, when this window is it.
    pub quick: Option<quick::Panel>,
    /// The quick terminal's chord, which the main window registers.
    pub quick_chord: quick::Chord,
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
        Self::with_preferences(resolved, machine, prefs)
    }

    pub(crate) fn with_preferences(
        resolved: &Resolved,
        machine: Option<String>,
        prefs: rune::prefs::Prefs,
    ) -> Self {
        let (config, file_error) = settings::load_file();
        let file = config.rune;
        // `defaults.attach-session` opens this computer's server unless a
        // flag or link named a session.
        let mut launch = resolved.clone();
        if machine.is_none() && launch.session.is_none() {
            launch.session = config
                .defaults
                .attach_session
                .filter(|s| !s.trim().is_empty());
        }
        let key_problems = crate::keys::bind(&file.keys, file.key_defaults != Some(false));
        phux_client_runtime::session_order::set_recent_first(file.recent_first.unwrap_or(false));
        let file_error = file_error.or_else(|| {
            (!key_problems.is_empty()).then(|| format!("[rune.keys] {}", key_problems.join("; ")))
        });
        let font_size = file
            .font_size
            .or_else(|| prefs.get::<f32>("font_size"))
            .map_or(grid::DEFAULT_FONT_SIZE, |s| {
                s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
            });
        let interface_size = settings::interface_size(
            file.interface_size
                .or_else(|| prefs.get::<u16>("interface_size")),
        );
        let font_family: String = (file.font_family.clone())
            .or_else(|| prefs.get("font_family"))
            .unwrap_or_default();
        let sidebar_open = prefs.get("sidebar_open").unwrap_or(true);
        let settings = settings::Settings::load(&prefs).over(&file);
        let mut desk = Self {
            connection: connection::Connection::default(),
            pending: None,
            notice: None,
            toasts: notice::Toasts::default(),
            input_refusals: 0,
            accessibility: accessibility::Accessibility::load(&prefs),
            palette_open: false,
            palette_query: rune::TextEdit::new(""),
            palette_picked: 0,
            palette_mode: crate::palette::Mode::All,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            plugin_actions: plugin_actions::Actions::default(),
            theme_before: None,
            sidebar_open,
            window_w: 0.0,
            window_h: 0.0,
            sidebar_peek: false,
            pane_numbers: Vec::new(),
            sync_tab: None,
            pointer: links::Pointer::default(),
            inbox: inbox::Inbox::default(),
            galaxy: galaxy::Galaxy::default(),
            reef: reef::Reef::default(),
            pitch: pitch::Pitch::default(),
            kitchen: kitchen::Kitchen::default(),
            pitlane: pitlane::PitLane::default(),
            hive: hive::Hive::default(),
            playground: None,
            fleet: fleet::Fleet::default(),
            settings,
            settings_open: false,
            onboarding: onboarding::Onboarding::default(),
            news: news::News::load(&prefs),
            settings_ui: settings::SettingsUi::default(),
            sidebar_collapsed: HashSet::new(),
            updater: updates::Updater::new(&prefs, resolved),
            machines: machines::Machines::new(machine, resolved.phux.clone()),
            link: link::Link::default(),
            launch,
            waker: None,
            prefs,
            onboarding_dir: onboarding::default_state_dir(),
            activities: crate::activity::Activities::default(),
            blink_on: true,
            blink_since: 0.0,
            size_seen: None,
            size_until: 0.0,
            size_was_shown: false,
            clock: 0.0,
            generation: 0,
            font_size,
            interface_size,
            font_due: !font_family.is_empty(),
            font_family,
            font_epoch: rune::text::font_epoch(),
            scale: Cell::new(2.0),
            system_dark: Cell::new(None),
            window_title: String::new(),
            focused: true,
            phone: None,
            bare: None,
            quick: None,
            quick_chord: quick::Chord::default(),
            native: None,
            file,
        };
        if let Some(error) = file_error {
            desk.say(NoticeKind::Error, error);
        }
        desk
    }

    /// The view's zoom: the interface size, except on a phone.
    pub fn ui_zoom(&self) -> f32 {
        if self.phone.is_some() {
            return 1.0;
        }
        f32::from(self.interface_size) / 100.0
    }

    /// The terminal's cells. `scale` is the zoomed view's, so the cells are
    /// measured at the window's and carried into the zoom unchanged.
    pub fn metrics(&self) -> Metrics {
        let k = self.ui_zoom();
        Metrics::new(self.font_size, self.scale.get() / k).zoomed(k)
    }

    /// Drain the runtime once. True when anything on screen may have changed.
    pub fn poll(&mut self, clock: f64, tx: &mut Tx) -> bool {
        self.clock = clock;
        self.follow_font(tx);
        self.machines.tick_glances(clock, tx);
        self.follow_path(tx);
        let updated = self.pump_updates(tx)
            | self.finish_document_images()
            | self.finish_document_viewers(tx)
            | self.pump_machines()
            | self.pump_checklist()
            | self.tick_fleet()
            | self.pump_link()
            | self.pump_links()
            | self.finish_screenshot()
            | self.pump_settings(tx)
            | self.pump_onboarding(tx)
            | self.follow_accessibility()
            | self.expire_notice(tx);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let updated = updated | self.pump_plugin_actions();
        let Some(client) = self.connection.client.clone() else {
            return updated | self.tend_link(tx);
        };
        let (news, moved) = self.observe(&client, client.take_activity(), tx);
        let mut changed = updated | news | self.take_globals(&client);
        let rearrange = updated || moved;
        changed |= self.drop_dead_views(&client) | self.tend_popup(&client, tx);
        let opened = self.settle_home(&client) | self.open_wanted(&client);
        changed |= opened;
        let unfocused = self.sync_focus(&client);
        self.title(tx);
        self.handoff();
        if rearrange || opened || self.layout_is_stale() {
            changed |= self.refresh_layout(&client);
        }
        self.reveal_active_tab(tx);
        // The replica's news may never come, and a split only waits so long.
        if unfocused || self.splits_waiting() {
            tx.wake_after(Duration::from_millis(250));
        }
        changed |= self.finish_uploads(&client);
        changed |= self.take_paths(&client) | self.take_screens();
        changed |= self.take_changes(&client);
        changed |= self.sync_alerts();
        changed |= self.tick_inbox(tx);
        changed |= self.tick_playground(tx);
        self.settle_pointer();
        changed |= self.autoscroll(tx);
        changed |= self.phone_touch_tick(tx);
        self.keep_find();
        self.keep_copy_mode();
        // Also re-snaps cells after a display-scale change.
        self.fit();
        self.wake_phone_fit(tx);
        changed | self.blink(&client, tx) | self.glance_size(&client, tx) | self.tend_link(tx)
    }
}

#[cfg(test)]
mod preferences_tests;

/// A topology of just `sessions` and `panes`.
#[cfg(test)]
pub fn topology(
    sessions: Vec<phux_client_runtime::control::SessionDescriptor>,
    panes: Vec<phux_client_runtime::control::PaneDescriptor>,
) -> phux_client_runtime::control::Topology {
    phux_client_runtime::control::Topology {
        sessions,
        resources: Vec::new(),
        panes,
        agent_sessions: Vec::new(),
        documents: Vec::new(),
        satellite_sessions: Vec::new(),
        focused_session: 0,
        focused_pane: ResourceId::local(0),
        authority: None,
        inbox: None,
        handles: phux_client_core::layout::wire::HandleMap::new(),
    }
}
