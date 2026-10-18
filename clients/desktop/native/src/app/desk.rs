//! One window: connection, workspace, and the command path shared by keys, menus, and controls.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, Context, CursorStyle, Entity, FocusHandle, KeyDownEvent, Pixels, Subscription,
    Task, Window,
};
use gpui_kit::component::command::CommandState;
use gpui_kit::component::input::InputState;
use phux_client_runtime::Client;
use phux_client_runtime::control::{DeliveryOutcome, Event, FileUploadReceipt, PathAnswer, Status};
use phux_client_runtime::engine::{Scroll, TextRegion};
use phux_protocol::ResourceId;
use phux_protocol::ids::{OperationId, SatelliteHost, SessionKey};

use crate::connection::path::{self as paths};
use crate::connection::{AgentInfo, AgentState, Connection, ConnectionIdentity};
use crate::settings::skin::{Backdrop, Shift, Skin};
use crate::settings::{self, DisplayPrefs, SidebarGrouping, TabPosition, WindowBackground};
use crate::terminal::{HostAction, Settings as TerminalSettings};
use crate::workspace::layout::{Axis, Direction, Placement};
use crate::workspace::persist::{self, FileLayoutStore};
use crate::workspace::{Workspace, WorkspaceOptions};

use super::actions::Run;
use super::host;
use super::keys::{gpui_stroke, shell_chord};
use super::motion::Motion;
use super::notifications::{Feed, Kind, SystemTag};
use super::overlays::{GotoFrecency, GotoPalette, GotoSearch, InputSlot};
use super::panes::Panes;
use super::session::Desktop;
use super::settings_panel::SettingsControls;
use super::skin::{Look, apply_backdrop};
use super::typed::typed_keys;

mod activity;
mod chrome;
mod commands;
mod input;
mod labels;
mod lifecycle;
mod prefs;
mod search;
mod sessions;
mod state;
#[cfg(test)]
mod tests;
mod work;

pub(crate) use labels::*;

const SEARCH_BYTES: usize = 8 * 1024 * 1024;
const UPLOAD_MOVED: &str = "The terminal changed before the upload started. Nothing was pasted.";

fn agent_attention(info: &AgentInfo) -> super::notifications::AgentAttention {
    super::notifications::AgentAttention {
        state: info.state,
        attention: info.attention,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowRole {
    #[default]
    Main,
    Fresh,
    Quick,
}

#[derive(Clone, Debug, Default)]
pub struct OpenRequest {
    pub role: WindowRole,
    pub session: String,
    pub home_key: Option<SessionKey>,
    /// Spawn a new terminal when nothing is restored, instead of showing the home pane.
    pub fresh: bool,
    pub initial_terminal: Option<ResourceId>,
    pub layout_path: PathBuf,
    pub legacy_layout: Option<PathBuf>,
    /// Default stores adopt the session key after the first topology; explicit
    /// layout paths and fresh windows keep their caller-selected store.
    pub stable_layout: bool,
    pub startup_error: Option<String>,
    /// The main window's display, which a quick window borrows.
    pub main_display: Option<DisplayPrefs>,
    /// A host from the remote registry. Absent is the local server.
    pub remote: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRenameTarget {
    pub key: Option<SessionKey>,
    pub id: u32,
    pub identity: ConnectionIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    None,
    Commands,
    Capture,
    Goto,
    Settings,
    Notifications,
    NativeLaunch,
    Path,
    MoveResources,
    Rename {
        tab_id: String,
    },
    Session {
        create: bool,
        current: String,
        target: Option<SessionRenameTarget>,
    },
    /// Kill a whole session: every member ends and the session is deleted.
    KillSession {
        name: String,
        session: u32,
    },
    WindowSession,
    /// Every open native window, its session, host and tabs.
    Windows,
    Terminate {
        terminal: ResourceId,
        title: String,
    },
    /// Register a machine over ssh with `phux host add`.
    AddHost,
    /// Forget a registered machine with `phux host rm`.
    RemoveHost {
        name: String,
    },
    /// A line-breaking paste held because the placement's terminal is
    /// running a command.
    Paste {
        placement_id: String,
        /// The terminal the paste was held for; a retargeted placement
        /// refuses it.
        terminal_id: ResourceId,
        title: String,
        paste: crate::input::HeldPaste,
    },
}

#[derive(Clone, Debug, Default)]
pub struct FindState {
    pub placement_id: String,
    pub query: String,
    pub case_sensitive: bool,
    pub matches: Vec<(u64, u64)>,
    pub index: usize,
}

pub struct Desk {
    pub desktop: Entity<Desktop>,
    pub(crate) updates: Entity<super::updates::Updater>,
    pub role: WindowRole,
    pub socket: PathBuf,
    /// A host from the remote registry this window attaches to instead of `socket`.
    pub remote: Option<String>,
    pub session: String,
    home_key: Option<SessionKey>,
    expected_authority: Option<phux_protocol::ids::AuthorityId>,
    pub phux: PathBuf,
    pub focus: FocusHandle,
    pub prefs: DisplayPrefs,
    pub ghostty_found: bool,
    /// The parsed Ghostty config, read at open and on `reload-config` only.
    ghostty: settings::GhosttyConfig,
    /// Built-in chords with Ghostty's binds layered on while `ghostty_keys`.
    keymap: Vec<(String, String)>,
    /// The chord each command shows, from `keymap`.
    chords: HashMap<String, Chord>,
    /// Paint values for the current preferences, skin, Ghostty config and keymap.
    paint: RefCell<Option<Rc<Paint>>>,
    pub modal: Modal,
    /// The modal whose controls were last built and focused.
    prepared: Modal,
    pub find: Option<FindState>,
    pub picker: Option<paths::PickerState>,
    pub feed: Feed,
    pub(crate) recent_projects: Vec<PathBuf>,
    pub link: Option<Link>,
    pub connect_error: Option<String>,
    pub layout_blocked: bool,
    pub persist_writes: bool,
    store: FileLayoutStore,
    locations: HashMap<PathBuf, FileLayoutStore>,
    saves: persist::writer::Writer,
    _save_errors: Task<()>,
    stable_layout: bool,
    layout_key: Option<SessionKey>,
    layout_migration_pending: bool,
    last_saved: Option<persist::SavedLayout>,
    panes: Panes,
    agent_seen: HashMap<ResourceId, super::notifications::AgentAttention>,
    agent_epoch: Option<u64>,
    /// Shell commands in flight, for the finished-in-the-background alert.
    commands: super::notifications::Commands,
    /// Server-held approvals in the Inbox (ADR-0128).
    pub(crate) approvals: super::approvals::ApprovalInbox,
    drag: Option<Drag>,
    /// Split container bounds from the last prepaint, keyed by split id.
    pub(crate) split_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// Painted tab centers, including scroll offsets, for the current orientation.
    pub(crate) tab_geometry: Rc<RefCell<TabGeometry>>,
    viewport: gpui::Size<gpui::Pixels>,
    _wake: Option<Task<()>>,
    /// Runtime state the desk chrome shows, as of the last activity drain.
    runtime_chrome: RuntimeChrome,
    _clock: Option<Task<()>>,
    /// When the armed clock wakes.
    clock_at: Option<Instant>,
    _subs: Vec<Subscription>,
    /// The list palette shared by commands, goto, sessions, and the path picker.
    pub(crate) palette_state: Option<Entity<CommandState>>,
    pub(super) palette_view: super::overlays::PaletteView,
    /// Go To state chips. A change keeps the query and realigns by entry.
    pub(crate) goto: GotoPalette,
    /// Folded Go To rows. Rebuilt when pane, agent, or tab metadata changes.
    pub(crate) goto_search: GotoSearch,
    /// Opens remembered for Go To ranking. Visits follow a listed destination only.
    pub(crate) goto_frecency: GotoFrecency,
    pub(crate) modal_inputs: Vec<(InputSlot, Entity<InputState>, Subscription)>,
    pub(crate) find_input: Option<(Entity<InputState>, Subscription)>,
    pub(crate) conversation_faces: HashMap<String, super::conversation::Face>,
    pub(crate) settings_controls: Option<SettingsControls>,
    pub(crate) settings_page: Option<super::settings_panel::Page>,
    pub(crate) native_launch: super::native_launch::State,
    pub(crate) capture: super::capture::State,
    /// The Changes panel beside the panes.
    pub(crate) changes: super::changes::Changes,
    /// `OpenRequest::fresh`, kept so a rebuilt link spawns the same way.
    open_fresh: bool,
    /// A CLI-opened terminal that has not appeared in this window's topology yet.
    pending_project_terminal: Option<ResourceId>,
    /// Sessions folded closed in the sidebar, by name.
    pub(crate) collapsed: std::collections::HashSet<String>,
    /// The remote registry as last read, for the sidebar's Hosts section.
    pub(crate) hosts: Vec<host::RemoteHost>,
    pub(crate) fleet: super::fleet::Fleet,
    /// A host enrollment in flight, by the target it was given.
    pub(crate) host_adding: Option<String>,
    /// The titlebar's window-drag surface, created once.
    pub(crate) drag_region: Option<Entity<crate::chrome::DragRegion>>,
    /// The glass menu showing, by the trigger that opened it.
    pub(super) menu: Option<super::menus::OpenMenu>,
    /// The trigger whose menu last closed, and when: a click that closes a
    /// menu by deactivating it must not reopen it from the same trigger.
    pub(super) menu_closed: Option<(gpui::SharedString, Instant)>,
    /// Where menu triggers were painted.
    pub(crate) anchors: super::ui::anchor::Anchors,
    /// Titlebar and sidebar. A terminal frame does not rebuild them.
    pub(super) chrome: super::view::ChromeCache,
    /// The window title last written.
    window_title: String,
    closed: bool,
    /// Session creates awaiting their outcome, by operation, with the name.
    creates: HashMap<OperationId, String>,
    renames: HashMap<OperationId, String>,
    /// Session kills awaiting their outcome, by operation, with the name.
    kills: HashMap<OperationId, String>,
    pub(crate) organization: super::organization::Organization,
    /// Panes growing in and folding away.
    pub(crate) motion: Motion,
    /// The skin the prefs resolve to, and the change into it still painting.
    skin_target: &'static Skin,
    shift: Option<Shift>,
    /// The window background last applied.
    backdrop: Option<Backdrop>,
    /// The placement whose top edge, or whose pane controls, the pointer is over.
    pub(crate) chrome_hover: Option<String>,
    pub(crate) controls_hover: Option<String>,
    pub(crate) pane_menu_open: Option<String>,
    /// The placement a file drag from the OS is over.
    pub(crate) drop_target: Option<String>,
    /// Uploads waiting for a receipt before any path is pasted.
    uploads: Vec<PendingUpload>,
    /// Bumped when the connection is replaced, so a late read does not upload.
    upload_epoch: u64,
    /// File reads still off the UI thread.
    uploads_reading: u32,
}

struct PendingUpload {
    transfer_id: u64,
    destination: crate::attach::UploadDestination,
    local_source: Option<PathBuf>,
}

pub struct Link {
    pub connection: Connection,
    pub workspace: Workspace,
}

enum Drag {
    Sidebar,
    Split {
        id: String,
        axis: Axis,
    },
    Tab {
        id: String,
        position: TabPosition,
        start: f32,
        moved: bool,
    },
}

#[derive(Default)]
pub(crate) struct TabGeometry {
    position: TabPosition,
    centers: Vec<f32>,
}

impl TabGeometry {
    pub(crate) fn prepare(&mut self, position: TabPosition, count: usize) {
        self.position = position;
        self.centers.clear();
        self.centers.resize(count, f32::NAN);
    }

    pub(crate) fn record(&mut self, index: usize, position: TabPosition, bounds: Bounds<Pixels>) {
        if self.position == position
            && let Some(center) = self.centers.get_mut(index)
        {
            *center = tab_coordinate(
                position,
                (bounds.origin.x + bounds.size.width / 2.).as_f32(),
                (bounds.origin.y + bounds.size.height / 2.).as_f32(),
            );
        }
    }

    fn drop_index(&self, from: usize, position: TabPosition, x: f32, y: f32) -> usize {
        if self.position != position {
            return from;
        }
        tab_drop_index(from, &self.centers, tab_coordinate(position, x, y))
    }
}

fn tab_coordinate(position: TabPosition, x: f32, y: f32) -> f32 {
    match position {
        TabPosition::Top | TabPosition::Bottom => x,
        TabPosition::Side => y,
    }
}

/// What the chrome shows of runtime state that changes in paint-only batches:
/// the "scrolled back" status chip and the "typing may not have arrived" pill.
/// Terminal views are notified alone for output, so a drain compares this and
/// notifies the desk only when it changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RuntimeChrome {
    scrolled_back: bool,
    fenced: Vec<ResourceId>,
}

/// Terminal damage, delivered input, and agent metadata that repeated every
/// badge (`agents_noted` false). Chrome, toasts, and layout stay put.
fn paint_batch(events: &[Event], agents_noted: bool) -> bool {
    !events.is_empty()
        && events.iter().all(|event| {
            crate::connection::paint_wake(event)
                || (!agents_noted && matches!(event, Event::AgentMetadata { .. }))
        })
}

/// The chord a command shows: as bound, and in menu glyphs.
struct Chord {
    bound: String,
    shown: String,
}

fn shown_chords(keymap: &[(String, String)]) -> HashMap<String, Chord> {
    settings::shown_chords(keymap, &settings::default_chords())
        .into_iter()
        .map(|(command, bound)| {
            let shown = settings::display_chord(&bound);
            (command, Chord { bound, shown })
        })
        .collect()
}

/// Paint values derived from the display preferences, the skin, the Ghostty
/// config and the keymap. A desk builds it once per change, not once per
/// frame; only a running skin shift rebuilds it every frame.
pub(crate) struct Paint {
    /// The preferences this paint was built from.
    prefs: DisplayPrefs,
    /// The skin this paint was built from.
    skin: Skin,
    /// The theme's palette, without the skin's washes.
    pub palette: crate::settings::theme::Palette,
    /// The skin resolved against the palette.
    pub look: Rc<Look>,
    /// Every terminal's settings except `focused` and `size_owner`.
    pub(super) terminal: TerminalSettings,
}

impl Paint {
    fn new(
        prefs: &DisplayPrefs,
        skin: &Skin,
        ghostty: &settings::GhosttyConfig,
        keymap: &[(String, String)],
    ) -> Self {
        let extra = settings::ghostty_theme(ghostty);
        let theme = crate::settings::theme::theme_by_id(&prefs.theme_id, extra.as_slice());
        let palette = crate::settings::theme::palette(&theme);
        let look = Rc::new(Look::of(skin, &palette, prefs));
        let from_hex = super::color::from_hex;
        let theme = &palette.theme;
        let terminal = TerminalSettings {
            font: gpui::font(prefs.font_family.clone()),
            font_size: prefs.font_size as f32,
            line_height: prefs.line_height as f32,
            cell_width_scale: prefs.cell_width as f32,
            cell_height_scale: prefs.cell_height as f32,
            foreground: Some(from_hex(&theme.foreground)),
            background: Some(look.card),
            host_fills_background: true,
            cursor: Some(from_hex(&theme.cursor)),
            selection_background: from_hex(&theme.selection),
            selection_foreground: from_hex(
                theme
                    .selection_foreground
                    .as_deref()
                    .unwrap_or(&theme.foreground),
            ),
            palette: theme.palette.as_ref().and_then(|entries| {
                if entries.len() != 16 {
                    return None;
                }
                let mut parsed = [gpui::Hsla::default(); 16];
                for (slot, color) in parsed.iter_mut().zip(entries) {
                    *slot = from_hex(color);
                }
                Some(parsed)
            }),
            cursor_visible: true,
            blink_visible: true,
            cursor_laser: prefs.cursor_laser && prefs.animations && skin.motion.cursor_effects,
            option_as_alt: prefs.option_as_alt,
            app_chords: settings::app_chords(keymap),
            ..TerminalSettings::default()
        };
        Self {
            prefs: prefs.clone(),
            skin: *skin,
            palette,
            look,
            terminal,
        }
    }
}

fn session_failure(create: bool) -> &'static str {
    if create {
        "Could not create the session"
    } else {
        "Could not rename the session"
    }
}

/// The modal a command opens, which the same command closes.
fn listed_modal(id: &str) -> Option<Modal> {
    Some(match id {
        "palette" => Modal::Commands,
        "capture" => Modal::Capture,
        "goto" => Modal::Goto,
        "windows" => Modal::Windows,
        "settings" => Modal::Settings,
        "notifications" => Modal::Notifications,
        _ => return None,
    })
}

/// `split-left` as (`split`, left).
fn directional(id: &str) -> Option<(&str, Direction)> {
    let (verb, word) = id.rsplit_once('-')?;
    let direction = match word {
        "left" => Direction::Left,
        "right" => Direction::Right,
        "up" => Direction::Up,
        "down" => Direction::Down,
        _ => return None,
    };
    Some((verb, direction))
}

/// The skin display preferences resolve to.
fn resolved_skin(prefs: &DisplayPrefs) -> &'static Skin {
    settings::skin_by_id(settings::resolved_id(&prefs.skin_id, prefs.plain))
}

impl gpui::Render for Desk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        super::view::render(self, window, cx)
    }
}

impl gpui::Focusable for Desk {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}
