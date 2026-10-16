//! One window: connection, workspace, and the command path shared by keys, menus, and controls.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
use crate::settings::{self, DisplayPrefs, TabPosition, WindowBackground};
use crate::terminal::{HostAction, Settings as TerminalSettings};
use crate::workspace::layout::{Axis, Direction, Placement};
use crate::workspace::persist::{self, FileLayoutStore};
use crate::workspace::{Workspace, WorkspaceOptions};

use super::actions::Run;
use super::host;
use super::keys::{gpui_stroke, shell_chord};
use super::motion::{self, Motion};
use super::notifications::{Feed, Kind};
use super::overlays::{GotoFrecency, GotoPalette, GotoSearch, InputSlot};
use super::panes::Panes;
use super::session::Desktop;
use super::settings_panel::SettingsControls;
use super::typed::typed_keys;

const SEARCH_BYTES: usize = 8 * 1024 * 1024;

fn agent_attention(info: &AgentInfo) -> super::notifications::AgentAttention {
    super::notifications::AgentAttention {
        state: info.state,
        attention: info.attention,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRole {
    Main,
    Fresh,
    Quick,
}

#[derive(Clone, Debug)]
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
}

#[derive(Clone, Debug)]
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
    /// Paint values for the current preferences, Ghostty config and keymap.
    look: RefCell<Option<Rc<Look>>>,
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
    drag: Option<Drag>,
    /// Split container bounds from the last prepaint, keyed by split id.
    pub(crate) split_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// Painted tab centers, including scroll offsets, for the current orientation.
    pub(crate) tab_geometry: Rc<RefCell<TabGeometry>>,
    viewport: gpui::Size<gpui::Pixels>,
    _wake: Option<Task<()>>,
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
    /// A host switch is resolving its target's home session.
    #[cfg(feature = "ui-fixtures")]
    pub(crate) host_switching: bool,
    /// The titlebar's window-drag surface, created once.
    pub(crate) drag_region: Option<Entity<crate::chrome::DragRegion>>,
    #[cfg(feature = "ui-fixtures")]
    pub(super) main_menu: Option<gpui::WindowHandle<super::glass_menu::GlassMenu>>,
    /// Titlebar and sidebar. A terminal frame does not rebuild them.
    pub(super) chrome: super::view::ChromeCache,
    /// The window title last written.
    window_title: String,
    /// The background the platform window has, starting from the one
    /// [`window_options_background`] opens it with.
    pub(super) window_background: WindowBackground,
    closed: bool,
    /// Session creates awaiting their outcome, by operation, with the name.
    creates: HashMap<OperationId, String>,
    renames: HashMap<OperationId, String>,
    /// Session kills awaiting their outcome, by operation, with the name.
    kills: HashMap<OperationId, String>,
    pub(crate) organization: super::organization::Organization,
    /// Panes growing in and folding away.
    pub(crate) motion: Motion,
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
        TabPosition::Top => x,
        TabPosition::Side => y,
    }
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

/// Paint values derived from the display preferences, the Ghostty config and
/// the keymap. A desk builds it once per change, not once per frame.
pub(crate) struct Look {
    /// The preferences this look was built from.
    prefs: DisplayPrefs,
    pub palette: crate::settings::theme::Palette,
    /// The palette with translucent washes and hairlines, so hovered and
    /// selected chrome tints the glass instead of covering it.
    pub chrome: crate::settings::theme::Palette,
    /// Every terminal's settings except `focused` and `size_owner`.
    pub(super) terminal: TerminalSettings,
}

impl Look {
    fn new(
        prefs: &DisplayPrefs,
        ghostty: &settings::GhosttyConfig,
        keymap: &[(String, String)],
    ) -> Self {
        let extra = settings::ghostty_theme(ghostty);
        let theme = crate::settings::theme::theme_by_id(&prefs.theme_id, extra.as_slice());
        let palette = crate::settings::theme::palette(&theme);
        let alpha = crate::settings::theme::with_alpha;
        let mut chrome = palette.clone();
        let ink = &palette.theme.foreground;
        chrome.hover = alpha(ink, 0.06);
        chrome.active = alpha(ink, 0.11);
        chrome.border = alpha(ink, 0.08);
        chrome.divider = alpha(ink, 0.08);
        let from_hex = super::color::from_hex;
        let theme = &palette.theme;
        let terminal = TerminalSettings {
            font: gpui::font(prefs.font_family.clone()),
            font_size: prefs.font_size as f32,
            line_height: prefs.line_height as f32,
            cell_width_scale: prefs.cell_width as f32,
            cell_height_scale: prefs.cell_height as f32,
            foreground: Some(from_hex(&theme.foreground)),
            background: Some(from_hex(&alpha(
                &theme.background,
                prefs.painted_terminal_opacity(),
            ))),
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
            option_as_alt: prefs.option_as_alt,
            app_chords: settings::app_chords(keymap),
            ..TerminalSettings::default()
        };
        Self {
            prefs: prefs.clone(),
            palette,
            chrome,
            terminal,
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl Desk {
    #[allow(
        clippy::too_many_arguments,
        reason = "window construction binds desktop handles, request, and its navigation snapshot"
    )]
    pub(crate) fn open(
        desktop: Entity<Desktop>,
        updates: Entity<super::updates::Updater>,
        phux: PathBuf,
        socket: PathBuf,
        request: OpenRequest,
        recent_projects: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store =
            FileLayoutStore::new(request.layout_path.clone(), request.legacy_layout.clone());
        let loaded = persist::load_layout(&store.read());
        let blocked = loaded.blocked;
        let saved = loaded.initial;
        let text = settings::read_ghostty_config();
        let ghostty = settings::parse_ghostty(text.as_deref().unwrap_or(""));
        let mut prefs = saved
            .as_ref()
            .map(|layout| layout.display.clone())
            .unwrap_or_else(|| {
                if text.is_some() {
                    settings::ghostty_prefs(&settings::default_display(), &ghostty)
                } else {
                    settings::default_display()
                }
            });
        if request.role == WindowRole::Quick {
            prefs = persist::quick_display(
                request.main_display.as_ref().or(Some(&prefs)),
                saved.as_ref(),
            );
        }
        prefs = settings::sanitize_display(&prefs);
        let keymap = settings::effective_keymap(&ghostty, prefs.ghostty_keys);
        let focus = cx.focus_handle();
        let saves = persist::writer::Writer::default();
        let errors = saves.errors();
        let save_errors = cx.spawn(async move |this, cx| {
            while let Ok(error) = errors.recv().await {
                let _ = this.update(cx, |desk, cx| {
                    desk.post(Kind::Error, "Could not save desktop layout", Some(error));
                    cx.notify();
                });
            }
        });
        let mut desk = Self {
            desktop,
            updates,
            role: request.role,
            socket,
            remote: request.remote.clone(),
            session: request.session.clone(),
            home_key: request.home_key,
            expected_authority: None,
            phux,
            focus,
            prefs,
            ghostty_found: text.is_some(),
            chords: shown_chords(&keymap),
            keymap,
            ghostty,
            look: RefCell::new(None),
            modal: Modal::None,
            prepared: Modal::None,
            find: None,
            picker: None,
            feed: Feed::default(),
            recent_projects,
            link: None,
            connect_error: None,
            layout_blocked: blocked,
            persist_writes: request.role != WindowRole::Fresh && !blocked,
            store,
            locations: HashMap::new(),
            saves,
            _save_errors: save_errors,
            stable_layout: request.stable_layout,
            layout_key: None,
            layout_migration_pending: false,
            last_saved: saved,
            panes: Panes::default(),
            agent_seen: HashMap::new(),
            agent_epoch: None,
            drag: None,
            split_bounds: Rc::default(),
            tab_geometry: Rc::default(),
            viewport: gpui::size(gpui::px(1320.), gpui::px(840.)),
            _wake: None,
            _clock: None,
            clock_at: None,
            _subs: Vec::new(),
            palette_state: None,
            palette_view: Default::default(),
            goto: GotoPalette::default(),
            goto_search: GotoSearch::default(),
            goto_frecency: GotoFrecency::default(),
            modal_inputs: Vec::new(),
            find_input: None,
            conversation_faces: HashMap::new(),
            settings_controls: None,
            settings_page: None,
            native_launch: Default::default(),
            open_fresh: request.fresh,
            pending_project_terminal: None,
            collapsed: Default::default(),
            hosts: Vec::new(),
            fleet: Default::default(),
            host_adding: None,
            #[cfg(feature = "ui-fixtures")]
            host_switching: false,
            drag_region: None,
            #[cfg(feature = "ui-fixtures")]
            main_menu: None,
            chrome: super::view::ChromeCache::default(),
            window_title: String::new(),
            window_background: WindowBackground::default(),
            closed: false,
            creates: HashMap::new(),
            renames: HashMap::new(),
            kills: HashMap::new(),
            organization: Default::default(),
            motion: Motion::default(),
            chrome_hover: None,
            controls_hover: None,
            pane_menu_open: None,
            drop_target: None,
            uploads: Vec::new(),
            upload_epoch: 0,
            uploads_reading: 0,
        };
        if let Some(error) = request.startup_error {
            desk.post(Kind::Error, "Could not start the phux server", Some(error));
        }
        if blocked {
            desk.post(
                Kind::Error,
                "Saved layout preserved",
                Some("This layout is invalid or from an unsupported version. Layout and settings changes will not be saved.".into()),
            );
        }
        desk.attach(request.initial_terminal, cx);
        desk.refresh_hosts(cx);
        desk.sync_bindings(cx);
        desk.apply_kit_theme(cx);
        let updates = desk.updates.clone();
        let mut runtime_generation = updates.read(cx).runtime_generation;
        desk._subs.push(cx.observe(&updates, move |_, updates, cx| {
            let generation = updates.read(cx).runtime_generation;
            if generation != runtime_generation {
                runtime_generation = generation;
                cx.spawn(async move |this, cx| {
                    smol::Timer::after(Duration::from_millis(500)).await;
                    let _ = this.update(cx, |desk, cx| desk.redial(cx));
                })
                .detach();
            }
            cx.notify();
        }));
        // Quit saves every window; a closed window saves through `Desktop::window_closed`.
        desk._subs.push(cx.on_app_quit(|desk, cx| {
            desk.prepare_close(cx);
            async {}
        }));
        // Window keys need the `Desk` context on the dispatch path before any terminal exists.
        window.focus(&desk.focus, cx);
        desk
    }

    fn attach(&mut self, initial: Option<ResourceId>, cx: &mut Context<Self>) {
        // Native launch completions belong to the client that initiated them.
        self.native_launch = Default::default();
        if self.session.is_empty() {
            // The launch could not name a session; its reason is already shown.
            self.connect_error = Some("No session to open".into());
            cx.notify();
            return;
        }
        let connected = match self.remote.as_deref() {
            Some(host) => {
                Connection::connect_remote_with_key(host, self.session.clone(), self.home_key)
            }
            None => Connection::connect_with_key(
                self.socket.clone(),
                self.session.clone(),
                self.home_key,
            ),
        };
        match connected {
            Ok(mut connection) => {
                connection.expect_authority(self.expected_authority);
                let mut workspace = Workspace::new(
                    self.session.clone(),
                    WorkspaceOptions {
                        fresh: self.open_fresh,
                        initial_terminal: initial,
                    },
                );
                if self.home_key.is_some() {
                    workspace.preserve_empty();
                }
                // A secondary window shows new terminals; every other window restores its own layout.
                if self.role != WindowRole::Fresh {
                    workspace.restore(self.last_saved.clone());
                }
                self.arm_wake(&connection, cx);
                self.link = Some(Link {
                    connection,
                    workspace,
                });
                self.connect_error = None;
            }
            Err(error) => {
                self.connect_error = Some(error.to_string());
                self.post(Kind::Error, "Could not connect", Some(error.to_string()));
            }
        }
        cx.notify();
    }

    fn arm_wake(&mut self, connection: &Connection, cx: &mut Context<Self>) {
        let rx = connection.activity();
        self._wake = Some(cx.spawn(async move |this, cx| {
            loop {
                if rx.recv().await.is_err() {
                    break;
                }
                if this.update(cx, |desk, cx| desk.on_activity(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn on_activity(&mut self, cx: &mut Context<Self>) {
        let _drain = crate::diag::DRAIN.timer();
        crate::diag::DRAIN_WAKES.incr();
        let (events, answers, uploads, epoch, agents_noted) = {
            let Some(link) = self.link.as_mut() else {
                return;
            };
            (
                link.connection.drain(),
                link.connection.take_path_answers(),
                link.connection.take_file_upload_receipts(),
                link.connection.connection_epoch(),
                link.connection.agents_noted(),
            )
        };
        crate::diag::DRAIN_EVENTS.add(events.len() as u64);
        let had_uploads = !uploads.is_empty();
        if had_uploads {
            self.finish_uploads(uploads);
        }
        let conversation_changed = self.conversation_receipts(&events);
        // Notify cached terminals even for mixed agent/PTY batches, but keep
        // the last frame while disconnected instead of reacquiring a view.
        if self.session_is_attached() {
            self.repaint_terminals(&events, cx);
        }
        // A spawn's bootstrap can arrive as a pure paint batch. Promote only
        // output that can actually settle an awaiting placement.
        let awaiting_output = self
            .link
            .as_ref()
            .is_some_and(|link| link.workspace.awaiting_output(&events));
        if answers.is_empty()
            && paint_batch(&events, agents_noted)
            && !conversation_changed
            && !awaiting_output
        {
            if had_uploads {
                cx.notify();
            }
            return;
        }
        self.apply_activity(&events, answers, epoch, cx);
    }

    fn apply_activity(
        &mut self,
        events: &[Event],
        answers: Vec<PathAnswer>,
        epoch: u64,
        cx: &mut Context<Self>,
    ) {
        self.adopt_session_layout(cx);
        self.sync_home_session(cx);
        self.note_activity(events, epoch);
        // A reexec resets layout placements, not the ACP owner or its editor.
        // Reopen its guardian before waiting for the exact fresh activation.
        self.remember_native_focus();
        if let Some(link) = self.link.as_mut() {
            link.workspace.handle(events, &link.connection);
            link.workspace.settle(&link.connection);
        }
        self.settle_project_terminal();
        self.reveal_pending_native(cx);
        self.take_notices();
        self.note_agents();
        self.flush_system(cx);
        for answer in answers {
            self.accept_path(answer);
        }
        self.retain_panes();
        self.retain_find();
        if self.modal == Modal::Path {
            self.pin_picker();
        }
        self.persist_if_changed();
        self.ensure_clock(cx);
        if events.iter().any(crate::connection::session_attached) {
            self.panes.notify_all(cx);
        }
        cx.notify();
    }

    fn note_activity(&mut self, events: &[Event], epoch: u64) {
        if self.agent_epoch != Some(epoch)
            || events.iter().any(crate::connection::starts_connection)
        {
            self.agent_seen.clear();
            self.feed.retain(|entry| entry.kind != Kind::Agent);
            self.agent_epoch = Some(epoch);
        }
        for event in events {
            if crate::connection::starts_connection(event) {
                self.organization.connection_changed();
            }
            self.note_event(event);
        }
    }

    fn settle_project_terminal(&mut self) {
        if let Some(terminal) = self.pending_project_terminal.as_ref()
            && self
                .link
                .as_ref()
                .is_some_and(|link| link.connection.pane(terminal).is_some())
            && let Some(terminal) = self.pending_project_terminal.take()
        {
            self.reveal_terminal(&terminal);
        }
    }

    fn retain_find(&mut self) {
        if self
            .find
            .as_ref()
            .is_some_and(|find| self.placement(find.placement_id.as_str()).is_none())
        {
            self.find = None;
            self.find_input = None;
        }
    }

    /// Output and delivered input paint the panes and leave the chrome tree in place.
    fn repaint_terminals(&mut self, events: &[Event], cx: &mut Context<Self>) {
        let terminals = events
            .iter()
            .filter_map(|event| match event {
                Event::TerminalChanged { terminal_id } => Some(terminal_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        self.panes.notify_terminals(&terminals, cx);
    }

    fn note_event(&mut self, event: &Event) {
        self.note_terminal_event(event);
        self.note_operation_event(event);
        self.note_input_event(event);
        if let Event::Detached { message, .. } = event {
            self.post(Kind::Error, "Detached", Some(message.clone()));
        }
    }

    fn sync_home_session(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let name = link.connection.session_name().to_owned();
        if let Some(topology) = &link.connection.topology {
            let desk = cx.entity_id();
            let local = self.remote.is_none();
            self.desktop.update(cx, |desktop, _| {
                if local {
                    desktop.observe_topology(topology);
                }
                desktop.sync_window_session(desk, &name, self.remote.as_deref(), self.role);
            });
        }
        if self.session == name {
            return;
        }
        self.session = name.clone();
        link.workspace.set_home_session(name.clone());
    }

    fn adopt_session_layout(&mut self, cx: &mut Context<Self>) {
        if !self.stable_layout || self.role == WindowRole::Fresh {
            return;
        }
        let Some(key) = self
            .link
            .as_ref()
            .and_then(|link| link.connection.home_key())
        else {
            return;
        };
        if self.layout_key == Some(key) {
            return;
        }
        self.layout_key = Some(key);
        let authority = self.remote.as_ref().map_or_else(
            || self.socket.display().to_string(),
            |host| format!("host:{host}"),
        );
        let paths = persist::session_layout_paths(&authority, key);
        let path = if self.role == WindowRole::Quick {
            paths.quick
        } else {
            paths.layout
        };
        // A preferences edit can still be writing the legacy source. Adoption
        // must not restore older bytes over the snapshot we just queued.
        self.flush_saves();
        let missing = matches!(
            FileLayoutStore::new(&path, None).read(),
            persist::StoreValue::Missing
        );
        let store = FileLayoutStore::new(path, Some(self.store.path.clone()));
        let loaded = persist::load_layout(&store.read());
        self.layout_blocked |= loaded.blocked;
        self.persist_writes &= !loaded.blocked;
        self.layout_migration_pending = missing && !self.layout_blocked;
        self.store = store;
        if loaded.blocked {
            self.post(
                Kind::Error,
                "Session layout is unreadable",
                Some("The saved file is preserved; layout writes are disabled.".to_owned()),
            );
            return;
        }
        if let Some(saved) = loaded.initial {
            self.prefs = if self.role == WindowRole::Quick {
                persist::quick_display(Some(&self.prefs), Some(&saved))
            } else {
                saved.display.clone()
            };
            self.last_saved = Some(saved.clone());
            self.workspace_mut(|link| link.workspace.restore(Some(saved)));
            self.sync_bindings(cx);
            self.apply_kit_theme(cx);
        }
    }

    fn note_operation_event(&mut self, event: &Event) {
        if let Event::OrganizationOperation { operation, outcome } = event {
            self.note_organization(*operation, outcome);
        }
        self.note_kill_event(event);
        self.note_create_event(event);
        self.note_rename_event(event);
    }

    fn note_terminal_event(&mut self, event: &Event) {
        if let Event::TerminalClosed {
            terminal_id,
            exit_status,
            signal,
            ..
        } = event
        {
            let views = self
                .link
                .as_ref()
                .map(|link| link.workspace.views_of(terminal_id))
                .unwrap_or(0);
            if views > 0 {
                let status = exit_status
                    .map(|code| format!("exit {code}"))
                    .or_else(|| signal.map(|code| format!("signal {code}")))
                    .unwrap_or_else(|| "closed".into());
                let title = self.pane_title(Some(terminal_id));
                self.post(Kind::Info, &format!("{title} ended"), Some(status));
            }
            self.agent_seen.remove(terminal_id);
            self.feed.retain(|entry| {
                entry.kind != Kind::Agent || entry.terminal.as_ref() != Some(terminal_id)
            });
        }
    }

    fn note_kill_event(&mut self, event: &Event) {
        if let Event::OrganizationOperation { operation, outcome } = event
            && let Some(name) = self.kills.remove(operation)
            && !matches!(outcome, phux_client_core::org_ops::OrgOutcome::Committed(_))
        {
            self.post(
                Kind::Error,
                &format!("Could not kill {name}"),
                Some(crate::connection::org::outcome_sentence(outcome)),
            );
        }
    }

    fn note_create_event(&mut self, event: &Event) {
        if let Event::OrganizationOperation { operation, outcome } = event
            && let Some(name) = self.creates.remove(operation)
            && !outcome.succeeded()
        {
            self.post(
                Kind::Error,
                "Could not create the session",
                Some(format!(
                    "{name}: {}",
                    crate::connection::org::outcome_sentence(outcome)
                )),
            );
        }
    }

    fn note_rename_event(&mut self, event: &Event) {
        if let Event::OrganizationOperation { operation, outcome } = event
            && let Some(name) = self.renames.remove(operation)
        {
            if outcome.succeeded() {
                self.post(Kind::Info, "Session renamed", Some(name));
            } else {
                self.post(
                    Kind::Error,
                    "Could not rename the session",
                    Some(crate::connection::org::outcome_sentence(outcome)),
                );
            }
        }
    }

    fn note_input_event(&mut self, event: &Event) {
        if let Some(message) = crate::connection::refused_input(event) {
            self.post(Kind::Error, "Input not delivered", Some(message));
        }
        if let Event::InputDelivery {
            outcome: DeliveryOutcome::Refused,
            message,
            ..
        } = event
            && crate::connection::refused_input(event).is_none()
            && !message.is_empty()
        {
            self.post(Kind::Error, "Input not delivered", Some(message.clone()));
        }
    }

    fn note_agents(&mut self) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let focused = link
            .workspace
            .focused()
            .and_then(|placement| placement.terminal_id.clone());
        let current = link.connection.agents.clone();
        // An agent entry stands while its agent still reports the state it announced.
        self.feed.retain(|entry| {
            entry.kind != Kind::Agent
                || entry
                    .terminal
                    .as_ref()
                    .and_then(|id| current.get(id))
                    .is_some_and(|info| {
                        entry.agent_state == Some(info.state) && agent_attention(info).actionable()
                    })
        });
        if !self.prefs.notifications {
            self.agent_seen = current
                .into_iter()
                .map(|(id, info)| (id, agent_attention(&info)))
                .collect();
            return;
        }
        for (id, info) in &current {
            let next = agent_attention(info);
            if !next.should_alert(self.agent_seen.get(id), focused.as_ref() == Some(id)) {
                continue;
            }
            let title = match info.word() {
                "" => format!("{} needs attention", info.name),
                word => format!("{} {word}", info.name),
            };
            let body = format!(
                "{} · {}",
                self.pane_title(Some(id)),
                short_path(self.pane_cwd(Some(id)))
            );
            self.feed.post_agent(title, body, id.clone(), info.state);
        }
        self.agent_seen = current
            .into_iter()
            .map(|(id, info)| (id, agent_attention(&info)))
            .collect();
    }

    fn take_notices(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        for notice in link.workspace.take_notices() {
            let kind = if notice.kind == "error" {
                Kind::Error
            } else {
                Kind::Info
            };
            self.post(kind, &notice.title, notice.body.clone());
        }
    }

    pub fn execute(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_field_has_keys() && settings::yields_to_text_field(id) {
            return;
        }
        let open = self.modal.clone();
        if !matches!(open, Modal::None) {
            let toggle = matches!(
                (&open, id),
                (Modal::Commands, "palette")
                    | (Modal::Goto, "goto")
                    | (Modal::Settings, "settings")
                    | (Modal::Notifications, "notifications")
            );
            self.close_modal(window, cx);
            if toggle {
                return;
            }
        }
        if let Some(classified) = settings::classify_command(id) {
            self.run_classified(classified, window, cx);
            return;
        }
        if !self.run_organization(id, window, cx) {
            self.run_catalog(id, window, cx);
        }
    }

    fn run_classified(
        &mut self,
        command: settings::BoundAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use settings::BoundAction::*;
        match command {
            Ignore => {}
            Send(text) => self.send_text(&text, cx),
            WriteFile { region, mode } => self.write_file(&region, &mode, window, cx),
            FontSize(size) => self.update_prefs(|prefs| prefs.font_size = size, cx),
            ScrollLines(rows) => self.scroll_lines(rows as i64, cx),
            PromptJump(prompts) => self.jump_prompt(prompts as i64, cx),
        }
    }

    fn run_catalog(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "palette" => self.modal = Modal::Commands,
            "goto" => self.modal = Modal::Goto,
            "settings" => self.modal = Modal::Settings,
            "notifications" => self.modal = Modal::Notifications,
            "clear-notifications" => self.feed.clear(),
            "new" => {
                self.workspace_call(|link| link.workspace.new_terminal(&link.connection, None))
            }
            "new-home" => {
                let home = std::env::var("HOME").ok();
                self.workspace_call(|link| {
                    link.workspace
                        .new_terminal(&link.connection, home.as_deref())
                });
            }
            "new-session" => {
                self.modal = Modal::Session {
                    create: true,
                    current: String::new(),
                    target: None,
                }
            }
            "rename-session" => self.open_rename_session(),
            "kill-session" => self.ask_kill_session(),
            "folder" => self.open_folder(window, cx),
            "native-agent" => self.open_native_launch(window, cx),
            "insert-path" => self.open_path_picker(),
            "split-right" => {
                self.workspace_call(|link| link.workspace.split(&link.connection, Axis::Row, false))
            }
            "split-down" => self
                .workspace_call(|link| link.workspace.split(&link.connection, Axis::Column, false)),
            "split-left" => {
                self.workspace_call(|link| link.workspace.split(&link.connection, Axis::Row, true))
            }
            "split-up" => self
                .workspace_call(|link| link.workspace.split(&link.connection, Axis::Column, true)),
            "view" => self.workspace_call(|link| link.workspace.duplicate_view(&link.connection)),
            "close" => self.fold_pane(window, cx),
            "close-tab" => {
                if let Some(id) = self.active_tab_id() {
                    self.workspace_mut(|link| link.workspace.close_tab(&link.connection, &id));
                }
            }
            "zoom" => self.workspace_mut(|link| link.workspace.toggle_zoom()),
            "equalize" => self.workspace_mut(|link| link.workspace.equalize_tab()),
            "pane-left" => {
                self.workspace_mut(|link| link.workspace.focus_direction(Direction::Left))
            }
            "pane-right" => {
                self.workspace_mut(|link| link.workspace.focus_direction(Direction::Right))
            }
            "pane-up" => self.workspace_mut(|link| link.workspace.focus_direction(Direction::Up)),
            "pane-down" => {
                self.workspace_mut(|link| link.workspace.focus_direction(Direction::Down))
            }
            "pane-next" => self.workspace_mut(|link| link.workspace.cycle_pane(1)),
            "pane-prev" => self.workspace_mut(|link| link.workspace.cycle_pane(-1)),
            "tab-next" => self.workspace_mut(|link| link.workspace.step_tab(1)),
            "tab-prev" => self.workspace_mut(|link| link.workspace.step_tab(-1)),
            "tab-move-left" => self.move_active_tab(-1),
            "tab-move-right" => self.move_active_tab(1),
            "rename" => self.rename_active(),
            "window" => self.move_to_window(cx),
            "find" => self.open_find(window, cx),
            "find-next" => self.step_find(1, window, cx),
            "find-prev" => self.step_find(-1, window, cx),
            "find-selection" => self.find_selection(window, cx),
            "find-close" => self.close_find(cx),
            "follow" | "scroll-bottom" => self.follow(cx),
            "paste-cwd" => self.paste_cwd(),
            "terminate" => self.ask_terminate(),
            "attention" => self.next_attention(),
            "sidebar" => {
                self.update_prefs(|prefs| prefs.sidebar_visible = !prefs.sidebar_visible, cx)
            }
            "tabs-top" => self.update_prefs(|prefs| prefs.tab_position = TabPosition::Top, cx),
            "tabs-side" => self.update_prefs(|prefs| prefs.tab_position = TabPosition::Side, cx),
            "font-up" => self.update_prefs(|prefs| prefs.font_size += 1.0, cx),
            "font-down" => self.update_prefs(|prefs| prefs.font_size -= 1.0, cx),
            "font-reset" => {
                let size = DisplayPrefs::default().font_size;
                self.update_prefs(|prefs| prefs.font_size = size, cx);
            }
            "scroll-page-up" => self.scroll_page(-1, cx),
            "scroll-page-down" => self.scroll_page(1, cx),
            "scroll-top" => self.scroll_top(cx),
            "clear" => self.send_text("\u{000c}", cx),
            "select-all" => self.select_all(cx),
            "copy" => self.host_action(HostAction::Copy, cx),
            "paste" => self.host_action(HostAction::Paste, cx),
            "paste-selection" => self.paste_selection(),
            "prompt-prev" => self.jump_prompt(-1, cx),
            "prompt-next" => self.jump_prompt(1, cx),
            "quick-terminal" => self.toggle_quick(cx),
            "new-window" => self.new_window(window, cx),
            "fullscreen" => window.toggle_fullscreen(),
            "maximize" => window.zoom_window(),
            "reload-config" => self.reload_ghostty(cx),
            "import-ghostty" => self.import_ghostty(cx),
            "reconnect" => self.reconnect(cx),
            "performance-monitor" | "performance-monitor-stop" => {
                // Opening/closing windows must wait until this desk's window lease ends.
                let desktop = self.desktop.clone();
                let start = id == "performance-monitor";
                cx.defer(move |cx| {
                    desktop.update(cx, |desktop, cx| {
                        if start {
                            if let Err(error) = desktop.open_performance(cx) {
                                log::error!("could not open performance monitor: {error:#}");
                            }
                        } else {
                            desktop.stop_performance(cx);
                        }
                    });
                });
            }
            "resize-left" => self.workspace_mut(|link| link.workspace.nudge(Direction::Left)),
            "resize-right" => self.workspace_mut(|link| link.workspace.nudge(Direction::Right)),
            "resize-up" => self.workspace_mut(|link| link.workspace.nudge(Direction::Up)),
            "resize-down" => self.workspace_mut(|link| link.workspace.nudge(Direction::Down)),
            other if other.starts_with("tab-") => {
                if let Ok(index) = other.trim_start_matches("tab-").parse::<isize>() {
                    let index = if index == 9 { -1 } else { index - 1 };
                    self.workspace_mut(|link| link.workspace.select_index(index));
                }
            }
            _ => {}
        }
        self.after_command(window, cx);
    }

    /// Every command and pointer action ends here: modal controls take the
    /// keyboard, or the terminal gets it back, and a changed layout is saved.
    pub(crate) fn after_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.take_notices();
        self.retain_panes();
        if self.modal != self.prepared {
            self.prepare_modal(window, cx);
        }
        self.focus_terminal(window, cx);
        self.sync_title(window);
        self.persist_if_changed();
        cx.notify();
    }

    /// Keys the window owns outside its key bindings. Escape closes the modal,
    /// else the find bar that holds the keys, and otherwise reaches the terminal.
    pub fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let plain = !modifiers.platform
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.shift
            && !modifiers.function;
        let key = event.keystroke.key.as_str();
        if plain && key == "escape" && self.conversation_escape(cx) {
            cx.stop_propagation();
            return;
        }
        if plain && key == "escape" && self.text_field_has_keys() {
            self.escape(window, cx);
            cx.stop_propagation();
            return;
        }
        if plain && key == "enter" {
            match self.modal {
                Modal::Terminate { .. } => {
                    self.confirm_terminate(window, cx);
                    cx.stop_propagation();
                    return;
                }
                Modal::KillSession { .. } => {
                    self.confirm_kill_session(window, cx);
                    cx.stop_propagation();
                    return;
                }
                Modal::RemoveHost { .. } => {
                    self.confirm_remove_host(window, cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if plain && key == "right" && self.modal == Modal::Path {
            self.open_selected_directory(window, cx);
            cx.stop_propagation();
            return;
        }
        let chord = shell_chord(
            modifiers.platform,
            modifiers.control,
            modifiers.alt,
            modifiers.shift,
            key,
            event.is_held,
        );
        if chord.is_empty() {
            return;
        }
        let settings_chord = settings::key_chord(&settings::KeyChord {
            key: key.to_owned(),
            held: event.is_held,
            cmd: modifiers.platform,
            alt: modifiers.alt,
            ctrl: modifiers.control,
            shift: modifiers.shift,
        });
        let chord = if settings_chord.is_empty() {
            chord
        } else {
            settings_chord
        };
        // Chords GPUI can name are key bindings already; only the rest arrive here.
        if gpui_stroke(&chord).is_some() {
            return;
        }
        if let Some(id) = self.binding(&chord) {
            window.dispatch_action(Box::new(Run::new(id)), cx);
            cx.stop_propagation();
        }
    }

    fn binding(&self, chord: &str) -> Option<String> {
        self.keymap
            .iter()
            .find(|(bound, _)| bound == chord)
            .map(|(_, id)| id.clone())
    }

    /// Recompute the keymap and hand it to the process key bindings and menus.
    /// Deferred: a desk is often opened from inside a `Desktop` update.
    pub fn sync_bindings(&mut self, cx: &mut Context<Self>) {
        self.keymap = settings::effective_keymap(&self.ghostty, self.prefs.ghostty_keys);
        self.chords = shown_chords(&self.keymap);
        // Every Ghostty config change resyncs the bindings, so the look follows it too.
        *self.look.get_mut() = None;
        let desktop = self.desktop.clone();
        let keymap = self.keymap.clone();
        let ghostty = self.ghostty.clone();
        let keys = self.prefs.ghostty_keys;
        cx.defer(move |cx| {
            desktop.update(cx, |desktop, cx| {
                desktop.install_bindings(keymap, &ghostty, keys, cx)
            });
        });
    }

    /// Kit controls (buttons, inputs, palette, settings) paint with the phux theme.
    pub(crate) fn apply_kit_theme(&self, cx: &mut Context<Self>) {
        let colors = self.palette();
        cx.defer(move |cx| super::color::apply_kit_theme(&colors, cx));
    }

    pub fn ghostty(&self) -> &settings::GhosttyConfig {
        &self.ghostty
    }

    pub(crate) fn keymap(&self) -> &[(String, String)] {
        &self.keymap
    }

    /// The chord `id` shows in menus and tooltips, in glyphs.
    pub fn shown_chord(&self, id: &str) -> Option<String> {
        self.chords.get(id).map(|chord| chord.shown.clone())
    }

    /// The chord `id` shows, as bound (`cmd+t`).
    pub(crate) fn bound_chord(&self, id: &str) -> Option<&str> {
        self.chords.get(id).map(|chord| chord.bound.as_str())
    }

    /// A modal, or the focused pane's find bar, owns the keyboard.
    pub(crate) fn text_field_has_keys(&self) -> bool {
        self.focused_placement()
            .is_some_and(|placement| self.conversation_visible(&placement.id))
            || settings::text_field_has_keys(
                !matches!(self.modal, Modal::None),
                self.find.as_ref().map(|find| find.placement_id.as_str()),
                self.focused_placement()
                    .map(|placement| placement.id.as_str()),
            )
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.modal, Modal::None) {
            self.close_modal(window, cx);
        } else if self.find.is_some() {
            self.close_find(cx);
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    pub fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Modal::Path) {
            self.picker = None;
        }
        // Closing the list is what reads it.
        if self.modal == Modal::Notifications {
            self.feed.read_all();
        }
        self.modal = Modal::None;
        self.prepared = Modal::None;
        self.modal_inputs.clear();
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// Build and focus the open modal's controls: a fresh query or prefilled field each time.
    fn prepare_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepared = self.modal.clone();
        self.modal_inputs.clear();
        super::overlays::prepare(self, window, cx);
    }

    /// The open modal changed in place (Go To became Commands) and keeps its controls.
    pub(crate) fn mark_prepared(&mut self) {
        self.prepared = self.modal.clone();
    }

    pub(crate) fn focused_cwd(&self) -> Option<String> {
        self.focused_pane().and_then(|pane| pane.cwd.clone())
    }

    pub(crate) fn workspace_call(&mut self, call: impl FnOnce(&mut Link)) {
        if let Some(link) = self.link.as_mut() {
            call(link);
        }
    }

    fn workspace_mut(&mut self, call: impl FnOnce(&mut Link)) {
        self.workspace_call(call);
    }

    /// Change, apply and save display preferences.
    pub(crate) fn update_prefs(
        &mut self,
        change: impl FnOnce(&mut DisplayPrefs),
        cx: &mut Context<Self>,
    ) {
        if self.apply_prefs(change, cx) {
            self.persist();
        }
    }

    /// Change and apply display preferences without writing them, for a
    /// control that moves continuously; [`Self::persist`] saves when it settles.
    pub(crate) fn apply_prefs(
        &mut self,
        change: impl FnOnce(&mut DisplayPrefs),
        cx: &mut Context<Self>,
    ) -> bool {
        let before = self.prefs.clone();
        change(&mut self.prefs);
        self.prefs = settings::sanitize_display(&self.prefs);
        if self.prefs == before {
            return false;
        }
        if self.prefs.tab_position != before.tab_position {
            if matches!(self.drag, Some(Drag::Tab { .. })) {
                self.drag = None;
            }
            self.tab_geometry
                .borrow_mut()
                .prepare(self.tab_position(), 0);
        }
        if self.prefs.ghostty_keys != before.ghostty_keys {
            self.sync_bindings(cx);
        }
        if self.prefs.theme_id != before.theme_id {
            self.apply_kit_theme(cx);
        }
        cx.notify();
        true
    }

    /// Write the layout and display. Tabs are written only once the attached
    /// server's restore has finished, so a half-restored window never replaces a saved layout.
    pub(crate) fn persist(&mut self) {
        if !self.persist_writes {
            return;
        }
        if let Some(next) = self.current_layout() {
            self.last_saved = Some(next);
        } else if let Some(saved) = self.last_saved.as_mut() {
            saved.display = self.prefs.clone();
        } else if self
            .link
            .as_ref()
            .and_then(|link| link.connection.server_id())
            .is_none()
        {
            self.last_saved = Some(persist::save_layout("", &[], &self.prefs, None));
        } else {
            return;
        }
        if let Some(saved) = &self.last_saved {
            self.queue_layout(saved.clone());
        }
    }

    fn queue_layout(&mut self, saved: persist::SavedLayout) {
        self.layout_migration_pending = false;
        self.saves.layout(self.store.clone(), saved.clone());
        self.last_saved = Some(saved);
    }

    fn flush_saves(&mut self) {
        for error in self.saves.flush() {
            self.post(Kind::Error, "Could not save desktop layout", Some(error));
        }
    }

    /// The layout this window shows, when it owns the attached server's restored tabs.
    fn current_layout(&self) -> Option<persist::SavedLayout> {
        let link = self.link.as_ref()?;
        let server = link.connection.server_id()?;
        let owned = link.connection.status == Status::Attached
            && link.workspace.restored()
            && link.workspace.server_id() == Some(server.as_str());
        owned.then(|| {
            persist::save_layout(
                &server,
                &link.workspace.tabs,
                &self.prefs,
                Some(link.workspace.active_id.as_str()),
            )
        })
    }

    /// Save after any workspace change: tabs, splits, ratios, focus, zoom, titles.
    pub(crate) fn persist_if_changed(&mut self) {
        if !self.persist_writes {
            return;
        }
        let Some(next) = self.current_layout() else {
            return;
        };
        if self.layout_migration_pending || self.last_saved.as_ref() != Some(&next) {
            self.queue_layout(next);
        }
    }

    /// Save, then release every view and the connection. Runs once per window.
    pub fn prepare_close(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.fleet.clear();
        #[cfg(feature = "ui-fixtures")]
        if let Some(menu) = self.main_menu.take() {
            let _ =
                gpui::AnyWindowHandle::from(menu).update(cx, |_, window, _| window.remove_window());
        }
        self.persist();
        self.flush_saves();
        self.abort_uploads(false);
        if let Some(link) = self.link.as_mut() {
            link.workspace.release_all(&link.connection);
            link.connection.close();
        }
        self.panes.clear();
        cx.notify();
    }

    /// Resolve the existing home before redialing; never recreate a stale name.
    fn prepare_reconnect_home(&mut self, cx: &mut Context<Self>) -> bool {
        if self
            .link
            .as_ref()
            .is_some_and(|link| link.connection.status == Status::Attached)
        {
            return true;
        }
        let key = self
            .link
            .as_ref()
            .and_then(|link| link.connection.home_key())
            .or(self.home_key);
        let result = if let Some(remote) = self.remote.as_deref() {
            crate::connection::home::existing_remote_home(remote, &self.session, key)
        } else {
            host::start_server(&self.phux, &self.socket).and_then(|()| {
                if self.session.is_empty() {
                    crate::connection::home::open_home(&self.phux, &self.socket, None)
                } else {
                    crate::connection::home::existing_home(&self.socket, &self.session, key)
                }
            })
        };
        match result {
            // A launch that could not name a session reopens on the one
            // the server now offers, with that session's saved layout.
            Ok(name) if self.session.is_empty() => {
                let old = cx.entity_id();
                let desktop = self.desktop.clone();
                cx.defer(move |cx| {
                    desktop.update(cx, |desktop, cx| desktop.rehome(old, name, cx));
                });
                false
            }
            Ok(name) => {
                self.session = name.clone();
                self.workspace_mut(|link| link.workspace.set_home_session(name));
                true
            }
            Err(error) => {
                self.post(Kind::Error, "Could not reconnect", Some(error));
                cx.notify();
                false
            }
        }
    }

    pub(crate) fn reconnect(&mut self, cx: &mut Context<Self>) {
        if !self.prepare_reconnect_home(cx) {
            return;
        }
        self.redial(cx);
    }

    /// Reattach after a managed handoff. The updater already performed the
    /// process work off-thread; this path never invokes a CLI subprocess.
    fn redial(&mut self, cx: &mut Context<Self>) {
        // Managed updates bypass the CLI resolver, but must not lose the
        // identity already learned by the old connection when rebuilding it.
        if let Some(key) = self
            .link
            .as_ref()
            .and_then(|link| link.connection.home_key())
        {
            self.home_key = Some(key);
        }
        self.persist();
        let snapshot = self.last_saved.clone();
        let closed = self
            .link
            .as_ref()
            .is_some_and(|link| matches!(link.connection.status, Status::Closed | Status::Failed));
        if closed || self.link.is_none() {
            self.abort_uploads(true);
            if let Some(link) = self.link.as_mut() {
                link.workspace.release_all(&link.connection);
                link.connection.close();
            }
            self.panes.clear();
            self.agent_seen.clear();
            self.feed.retain(|entry| entry.kind != Kind::Agent);
            self.find = None;
            self.find_input = None;
            self.attach(None, cx);
            if let Some(link) = self.link.as_mut() {
                link.workspace.restore(snapshot);
            }
        } else if let Some(link) = self.link.as_mut() {
            // The last frame stays. resync redials under it. Releasing the
            // views first is an empty window for the whole wait, and on a
            // slow link that wait is the reconnect.
            if let Err(error) = link.connection.reconnect() {
                self.post(Kind::Error, "Could not reconnect", Some(error.to_string()));
            }
        }
        cx.notify();
    }

    pub(crate) fn reload_ghostty(&mut self, cx: &mut Context<Self>) {
        let text = settings::read_ghostty_config();
        self.ghostty_found = text.is_some();
        self.ghostty = settings::parse_ghostty(text.as_deref().unwrap_or(""));
        self.sync_bindings(cx);
        self.apply_kit_theme(cx);
        if text.is_none() {
            self.post(Kind::Refusal, "No Ghostty config found", None);
        }
        cx.notify();
    }

    pub(crate) fn import_ghostty(&mut self, cx: &mut Context<Self>) {
        let next = settings::ghostty_prefs(&self.prefs, &self.ghostty);
        self.update_prefs(|prefs| *prefs = next, cx);
        self.apply_kit_theme(cx);
    }

    /// Deferred: the quick window's own desk may be the one asking.
    fn toggle_quick(&self, cx: &mut Context<Self>) {
        let desktop = self.desktop.clone();
        cx.defer(move |cx| desktop.update(cx, |desktop, cx| desktop.toggle_quick(cx)));
    }

    fn move_to_window(&mut self, cx: &mut Context<Self>) {
        let Some(placement) = self.focused_placement().cloned() else {
            return;
        };
        let Some(terminal) = placement.terminal_id.clone() else {
            return;
        };
        let session = self.session.clone();
        self.workspace_call(|link| {
            link.workspace
                .close_pane(&link.connection, Some(placement.id.as_str()))
        });
        let remote = self.remote.clone();
        self.desktop.update(cx, |desktop, cx| {
            desktop.open_moved(session, terminal, remote, cx);
        });
    }

    fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let desk = cx.entity().downgrade();
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            let paths = prompt
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .unwrap_or_default();
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = desk.update_in(cx, |desk, window, cx| {
                desk.open_project_path(path, window, cx);
            });
        })
        .detach();
    }

    /// A selected folder and a sidebar project use the same CLI/trust path.
    pub fn open_project_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(failure) = self.local_only() {
            self.post(Kind::Refusal, "Could not open folder", Some(failure));
            cx.notify();
            return;
        }
        let desk = cx.entity().downgrade();
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let navigation = self.upload_epoch;
        cx.spawn_in(window, async move |_, cx| {
            // Validation, trust checks and recipe launches can block.
            let result = smol::unblock(move || host::open_project(&phux, &socket, &path)).await;
            let _ = desk.update(cx, |desk, cx| match result {
                Ok(opened) => {
                    if let Err(error) = desk.desktop.update(cx, |desktop, cx| {
                        desktop.record_recent_project(opened.path, cx)
                    }) {
                        desk.post(
                            Kind::Error,
                            "Could not save recent folder",
                            Some(error.to_string()),
                        );
                    }
                    if desk.upload_epoch == navigation {
                        desk.open_project_window(opened.session, opened.terminal, cx);
                    } else {
                        desk.post(Kind::Info, "Project ready on Local", Some(opened.session));
                        cx.notify();
                    }
                }
                Err(error) => {
                    desk.post(Kind::Error, "Could not open folder", Some(error));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn forget_recent_project(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if let Err(error) = self
            .desktop
            .update(cx, |desktop, cx| desktop.forget_recent_project(path, cx))
        {
            self.post(
                Kind::Error,
                "Could not forget folder",
                Some(error.to_string()),
            );
        }
        cx.notify();
    }

    pub(crate) fn show_project_terminal(&mut self, terminal: ResourceId, cx: &mut Context<Self>) {
        if self
            .link
            .as_ref()
            .is_some_and(|link| link.connection.pane(&terminal).is_some())
        {
            self.pending_project_terminal = None;
            self.reveal_terminal(&terminal);
        } else {
            self.pending_project_terminal = Some(terminal);
            if let Some(link) = self.link.as_ref() {
                let _ = link.connection.refresh_topology();
            }
        }
        cx.notify();
    }

    pub fn open_project_window(
        &mut self,
        session: String,
        terminal: ResourceId,
        cx: &mut Context<Self>,
    ) {
        if self.remote.is_some() || self.session != session {
            let key = self
                .work_catalog()
                .into_iter()
                .find(|work| work.destination.host.is_none() && work.destination.name == session)
                .and_then(|work| work.destination.key);
            self.adopt_work(None, session, key, None, cx);
        }
        self.show_project_terminal(terminal, cx);
    }

    fn open_rename_session(&mut self) {
        let (name, target) = self
            .focused_pane()
            .map(|pane| {
                (
                    pane.session_name.clone(),
                    self.rename_session_target(pane.session_id),
                )
            })
            .unwrap_or_default();
        if name.trim().is_empty() {
            self.post(
                Kind::Refusal,
                "No session",
                Some("Focus a pane in the session you want to rename.".into()),
            );
            return;
        }
        self.modal = Modal::Session {
            create: false,
            current: name,
            target,
        };
    }

    pub(crate) fn rename_session_target(&self, id: u32) -> Option<SessionRenameTarget> {
        let connection = &self.link.as_ref()?.connection;
        let session = connection
            .topology
            .as_ref()?
            .sessions
            .iter()
            .find(|session| session.id == id)?;
        Some(SessionRenameTarget {
            key: session.key,
            id,
            identity: connection.identity()?,
        })
    }

    fn rename_active(&mut self) {
        if let Some(tab) = self.active_tab() {
            self.modal = Modal::Rename {
                tab_id: tab.id.clone(),
            };
        }
    }

    fn ask_terminate(&mut self) {
        let Some(placement) = self.focused_placement() else {
            return;
        };
        let Some(terminal) = placement.terminal_id.clone() else {
            return;
        };
        let title = self.pane_title(Some(&terminal));
        self.modal = Modal::Terminate { terminal, title };
    }

    pub fn confirm_terminate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Modal::Terminate { terminal, .. } = self.modal.clone() else {
            return;
        };
        self.close_modal(window, cx);
        self.workspace_call(|link| {
            link.workspace.terminate(&link.connection, &terminal);
        });
        self.after_command(window, cx);
    }

    pub fn apply_rename(&mut self, title: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Modal::Rename { tab_id } = self.modal.clone() {
            self.workspace_mut(|link| link.workspace.rename_tab(&tab_id, &title));
        }
        self.close_modal(window, cx);
        self.after_command(window, cx);
    }

    /// Create or rename a session. A refusal before anything is sent keeps
    /// the dialog open with a notification. A create on an `ORGANIZATION` server is
    /// persistent and reports its outcome when the server answers.
    pub fn apply_session(
        &mut self,
        name: String,
        directory: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Modal::Session {
            create,
            current,
            target,
        } = self.modal.clone()
        else {
            return;
        };
        if !create && self.rename_organization_session(target.as_ref(), &name, window, cx) {
            return;
        }
        if !create && !self.legacy_rename_matches(target.as_ref(), &current) {
            self.post(
                Kind::Error,
                "Session changed",
                Some("Reopen the rename dialog to select its current identity.".to_owned()),
            );
            cx.notify();
            return;
        }
        let directory = directory.unwrap_or_default();
        if create && self.create_organization_session(&name, &directory, window, cx) {
            return;
        }
        self.apply_legacy_session(create, &current, &name, &directory, window, cx);
    }

    fn create_organization_session(
        &mut self,
        name: &str,
        directory: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(link) = self
            .link
            .as_ref()
            .filter(|link| link.connection.organization())
        else {
            return false;
        };
        let submitted = match crate::workspace::create_session_error(name, directory) {
            Some(refused) => Err(refused.to_owned()),
            None => link
                .connection
                .create_session(name.trim(), directory.trim()),
        };
        match submitted {
            Ok(operation) => {
                self.creates.insert(operation, name.trim().to_owned());
                self.close_modal(window, cx);
                self.after_command(window, cx);
            }
            Err(failure) => {
                self.post(Kind::Error, "Could not create the session", Some(failure));
                cx.notify();
            }
        }
        true
    }

    fn apply_legacy_session(
        &mut self,
        create: bool,
        current: &str,
        name: &str,
        directory: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let failure = if let Some(failure) = self.local_only() {
            Some(failure)
        } else if create {
            host::create_session(&self.phux, &self.socket, name, directory)
        } else {
            host::rename_session(&self.phux, &self.socket, current, name)
        };
        if let Some(failure) = failure {
            self.post(
                Kind::Error,
                if create {
                    "Could not create the session"
                } else {
                    "Could not rename the session"
                },
                Some(failure),
            );
            cx.notify();
            return;
        }
        if let Some(link) = self.link.as_ref() {
            let _ = link.connection.refresh_topology();
        }
        self.close_modal(window, cx);
        self.after_command(window, cx);
    }

    fn rename_organization_session(
        &mut self,
        target: Option<&SessionRenameTarget>,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(link) = self
            .link
            .as_ref()
            .filter(|link| link.connection.organization())
        else {
            return false;
        };
        let submitted = target
            .and_then(|target| target.key)
            .ok_or_else(|| {
                "Session identity is unavailable. Reopen the rename dialog after refreshing."
                    .to_owned()
            })
            .and_then(|key| {
                link.connection
                    .client
                    .rename_session(key, name.trim())
                    .map_err(|error| error.to_string())
            });
        match submitted {
            Ok(operation) => {
                self.renames.insert(operation, name.trim().to_owned());
                self.close_modal(window, cx);
                self.after_command(window, cx);
            }
            Err(error) => {
                self.post(Kind::Error, "Could not rename the session", Some(error));
                cx.notify();
            }
        }
        true
    }

    fn legacy_rename_matches(&self, target: Option<&SessionRenameTarget>, name: &str) -> bool {
        let Some(target) = target else { return false };
        let Some(link) = self.link.as_ref() else {
            return false;
        };
        link.connection.status == Status::Attached
            && link.connection.identity().as_ref() == Some(&target.identity)
            && link.connection.topology.as_ref().is_some_and(|topology| {
                topology
                    .sessions
                    .iter()
                    .any(|session| session.id == target.id && session.name == name)
            })
    }

    /// Ask to kill the focused pane's session.
    fn ask_kill_session(&mut self) {
        let Some((name, session)) = self
            .focused_pane()
            .map(|pane| (pane.session_name.clone(), pane.session_id))
            .filter(|(name, _)| !name.trim().is_empty())
        else {
            self.post(
                Kind::Refusal,
                "No session",
                Some("Focus a pane in the session you want to kill.".into()),
            );
            return;
        };
        if self.intake_kill_refused(session) {
            return;
        }
        self.modal = Modal::KillSession { name, session };
    }

    pub fn confirm_kill_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Modal::KillSession { name, session } = self.modal.clone() else {
            return;
        };
        self.close_modal(window, cx);
        self.kill_session(&name, session);
        self.after_command(window, cx);
    }

    /// Kill `session` with one `SESSION_KILL`; a server without it gets
    /// `phux kill`.
    fn kill_session(&mut self, name: &str, session: u32) {
        if self.intake_kill_refused(session) {
            return;
        }
        let Some(link) = self.link.as_ref() else {
            self.post(
                Kind::Error,
                "Could not kill the session",
                Some("Not connected.".into()),
            );
            return;
        };
        let key = link
            .connection
            .topology
            .as_ref()
            .and_then(|topology| topology.sessions.iter().find(|s| s.id == session))
            .and_then(|descriptor| descriptor.key);
        if let Some(operation) = key.and_then(|key| link.connection.kill_session(key)) {
            self.kills.insert(operation, name.to_owned());
            return;
        }
        if let Some(failure) = self
            .local_only()
            .or_else(|| host::kill_session(&self.phux, &self.socket, name))
        {
            self.post(Kind::Error, "Could not kill the session", Some(failure));
        } else {
            let _ = link.connection.refresh_topology();
        }
    }

    /// A window on the focused pane's session; a choice only when there is one.
    fn new_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sessions = self
            .link
            .as_ref()
            .and_then(|link| link.connection.topology.as_ref())
            .map_or(0, |topology| topology.sessions.len());
        if sessions > 1 {
            self.modal = Modal::WindowSession;
            return;
        }
        let name = self
            .focused_pane()
            .map(|pane| pane.session_name.clone())
            .unwrap_or_default();
        self.open_session_window(name, window, cx);
    }

    fn intake_kill_refused(&mut self, session: u32) -> bool {
        if !self
            .link
            .as_ref()
            .is_some_and(|link| link.connection.is_intake_session(session))
        {
            return false;
        }
        self.post(Kind::Info, "Intake cannot be killed", Some("Move its work to a named session, or terminate individual processes. Intake is the designated home for unorganized work.".to_owned()));
        true
    }

    pub fn open_session_window(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(window, cx);
        let session = if name.trim().is_empty() {
            self.session.clone()
        } else {
            name
        };
        let remote = self.remote.clone();
        self.desktop
            .update(cx, |desktop, cx| desktop.open_session(session, remote, cx));
        cx.notify();
    }

    fn move_active_tab(&mut self, step: i8) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let len = link.workspace.tabs.len();
        if len < 2 {
            return;
        }
        let from = link
            .workspace
            .tabs
            .iter()
            .position(|tab| tab.id == link.workspace.active_id);
        let Some(from) = from else { return };
        let to = (from as i32 + step as i32).rem_euclid(len as i32) as usize;
        self.workspace_mut(|link| link.workspace.move_tab(from, to));
    }

    fn next_attention(&mut self) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let rank_of = |agent: &AgentInfo| crate::connection::state_rank(agent.state);
        let working = crate::connection::state_rank(AgentState::Working);
        let mut panes: Vec<_> = link
            .connection
            .panes()
            .iter()
            .filter(|pane| {
                link.connection
                    .agents
                    .get(&pane.terminal_id)
                    .is_some_and(|agent| rank_of(agent) <= working)
            })
            .cloned()
            .collect();
        panes.sort_by(|left, right| {
            let rank = |pane: &phux_client_runtime::control::PaneDescriptor| {
                link.connection
                    .agents
                    .get(&pane.terminal_id)
                    .map_or(9, rank_of)
            };
            rank(left).cmp(&rank(right)).then_with(|| {
                let changed = |pane: &phux_client_runtime::control::PaneDescriptor| {
                    link.connection
                        .agents
                        .get(&pane.terminal_id)
                        .map(|agent| agent.changed_at_ms)
                        .unwrap_or(0)
                };
                changed(right).cmp(&changed(left))
            })
        });
        if let Some(pane) = panes.first() {
            self.reveal_terminal(&pane.terminal_id);
        } else {
            self.post(Kind::Refusal, "No agent needs you", None);
        }
    }

    pub fn reveal_terminal(&mut self, terminal: &ResourceId) {
        let label = self.link.as_ref().and_then(|link| {
            link.workspace.tabs.iter().find_map(|tab| {
                crate::workspace::layout::placements(&tab.root)
                    .into_iter()
                    .find(|placement| placement.terminal_id.as_ref() == Some(terminal))
                    .map(|placement| placement.terminal_label.clone())
            })
        });
        self.workspace_call(|link| {
            if let Some(pane) = link.connection.pane(terminal).cloned() {
                link.workspace.open(&link.connection, &pane);
            } else if let Some(label) = label {
                let _ = link.workspace.reveal(&label);
            }
        });
    }

    pub fn select_tab(&mut self, id: &str) {
        self.workspace_mut(|link| link.workspace.select_tab(id));
    }

    pub fn close_tab(&mut self, id: &str) {
        self.workspace_mut(|link| link.workspace.close_tab(&link.connection, id));
    }

    pub fn focus_placement(
        &mut self,
        tab_id: &str,
        placement_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_mut(|link| link.workspace.focus_placement(tab_id, placement_id));
        self.after_command(window, cx);
    }

    /// Give the keyboard to whatever owns it: the open modal's control, the
    /// focused pane's find field, its terminal, or the window itself.
    fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.modal, Modal::None) {
            return;
        }
        if self.text_field_has_keys()
            && let Some((input, _)) = self.find_input.as_ref()
        {
            let input = input.clone();
            input.update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        if let Some(id) = self
            .focused_placement()
            .map(|placement| placement.id.clone())
            && self.focus_conversation(&id, window, cx)
        {
            return;
        }
        match self
            .focused_placement()
            .filter(|placement| placement.view_id.is_some())
            .map(|placement| placement.id.clone())
        {
            Some(id) if self.panes.get(&id).is_some() => self.panes.focus(&id, window, cx),
            _ => window.focus(&self.focus, cx),
        }
    }

    fn host_action(&mut self, action: HostAction, cx: &mut Context<Self>) {
        if let Some(id) = self
            .focused_placement()
            .filter(|placement| placement.view_id.is_some())
            .map(|placement| placement.id.clone())
            && let Some(notice) = self.panes.request(&id, action, cx)
        {
            self.post(Kind::Refusal, notice.title, Some(notice.body));
        }
    }

    fn send_text(&mut self, text: &str, _cx: &mut Context<Self>) {
        let Some(terminal) = self.focused_terminal() else {
            return;
        };
        if self.refuse_native_input(&terminal) {
            return;
        }
        let events = typed_keys(text);
        let client = self.client();
        let Some(client) = client else { return };
        let sent = client.with_control(|control| {
            if crate::input::native_owned(control, &terminal) {
                return false;
            }
            events
                .into_iter()
                .all(|event| control.send_key(&terminal, event))
        });
        if !sent {
            self.post(
                Kind::Error,
                "Could not send keys",
                Some("The terminal did not accept the input.".into()),
            );
        }
        client.wake();
    }

    fn scroll_lines(&mut self, rows: i64, cx: &mut Context<Self>) {
        self.with_view(cx, |client, view| {
            let _ = client.scroll_view(view, Scroll::Delta(rows));
        });
    }

    fn scroll_page(&mut self, direction: i64, cx: &mut Context<Self>) {
        self.with_view(cx, |client, view| {
            let rows = client
                .acquire_view(view)
                .map(|frame| frame.rows)
                .unwrap_or(1);
            let _ = client.scroll_view(
                view,
                Scroll::Delta(direction * i64::from(rows.saturating_sub(1)).max(1)),
            );
        });
    }

    fn scroll_top(&mut self, cx: &mut Context<Self>) {
        self.with_view(cx, |client, view| {
            let _ = client.scroll_view(view, Scroll::Top);
        });
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        self.with_view(cx, |client, view| {
            let _ = client.follow_live_view(view);
        });
    }

    fn jump_prompt(&mut self, prompts: i64, cx: &mut Context<Self>) {
        self.with_view(cx, |client, view| {
            let _ = client.scroll_view(view, Scroll::Prompt(prompts));
        });
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.with_engine(cx, |engine, view| {
            let _ = engine.select_all_view(view);
        });
    }

    fn paste_selection(&mut self) {
        let Some(text) = self.selection_text() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.paste_text(&text);
    }

    fn paste_cwd(&mut self) {
        let cwd = self.focused_pane().and_then(|pane| pane.cwd.clone());
        let Some(cwd) = cwd else { return };
        self.paste_text(&crate::attach::quote_paths(&[PathBuf::from(cwd)]));
    }

    /// Fold the focused pane away, then release it. A second close of a pane
    /// already folding does nothing.
    fn fold_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .focused_placement()
            .map(|placement| placement.id.clone())
        else {
            return;
        };
        if !self.motion.leave(&id, Instant::now()) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            smol::Timer::after(motion::LEAVE).await;
            let _ = this.update_in(cx, |desk, window, cx| {
                desk.motion.left(&id);
                desk.workspace_call(|link| link.workspace.close_pane(&link.connection, Some(&id)));
                desk.after_command(window, cx);
            });
        })
        .detach();
    }

    /// Paste dropped paths into the pane they were dropped on.
    /// A remote terminal uploads the files and pastes the server path.
    pub fn drop_paths(&mut self, placement_id: &str, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.drop_target = None;
        let Some(terminal) = self
            .placement(placement_id)
            .and_then(|placement| placement.terminal_id.clone())
        else {
            return;
        };
        self.paste_host_paths(&terminal, paths, cx);
    }

    /// Paste paths that live on this Mac, or upload them when the terminal does not.
    fn paste_host_paths(
        &mut self,
        terminal: &ResourceId,
        paths: &[PathBuf],
        cx: &mut Context<Self>,
    ) {
        if self.refuse_native_input(terminal) {
            return;
        }
        match crate::attach::attach_paths(&self.terminal_host(terminal), paths) {
            Some(crate::attach::PathAttach::Paste(text)) => {
                self.paste_text_to(terminal, &text);
            }
            Some(crate::attach::PathAttach::Upload(batch)) => {
                self.start_upload(terminal, batch, cx);
            }
            Some(crate::attach::PathAttach::Refuse { title, body }) => {
                self.post(Kind::Refusal, title, Some(body));
            }
            None => {}
        }
    }

    fn terminal_host(&self, terminal: &ResourceId) -> crate::attach::TerminalHost {
        crate::attach::terminal_host(self.remote.as_deref(), terminal)
    }

    /// A pending or retained native owner never turns back into a raw PTY.
    pub(super) fn native_owned_terminal(&self, terminal: &ResourceId) -> bool {
        let Some(link) = self.link.as_ref() else {
            return false;
        };
        self.native_launch
            .owns_terminal(&link.connection.client, terminal)
            || link.connection.conversations.values().any(|conversation| {
                conversation.parent == *terminal && conversation.native.is_some()
            })
            || link
                .connection
                .client
                .with_control(|control| crate::input::native_owned(control, terminal))
    }

    fn refuse_native_input(&mut self, terminal: &ResourceId) -> bool {
        if !self.native_owned_terminal(terminal) {
            return false;
        }
        self.post(
            Kind::Refusal,
            crate::attach::PASTE_REFUSED,
            Some(
                "Native ACP agents do not accept raw terminal input or file drops. Use the structured conversation. Nothing was sent."
                    .into(),
            ),
        );
        true
    }

    fn paste_text(&mut self, text: &str) -> bool {
        let Some(terminal) = self.focused_terminal() else {
            return false;
        };
        self.paste_text_to(&terminal, text)
    }

    /// Queue a bracketed paste. False, with a notification, when the terminal is not ready.
    fn paste_text_to(&mut self, terminal: &ResourceId, text: &str) -> bool {
        if self.refuse_native_input(terminal) {
            return false;
        }
        let Some(client) = self.client() else {
            return false;
        };
        let queued = client.with_control(|control| {
            if crate::input::native_owned(control, terminal) || !control.input_ready(terminal) {
                return false;
            }
            let _ = control.apply_paste(terminal, text);
            true
        });
        if queued {
            client.wake();
        } else {
            self.post(
                Kind::Error,
                "Could not paste",
                Some("The terminal is not ready.".into()),
            );
        }
        queued
    }

    /// Read local files off the UI thread, upload them, and paste nothing yet.
    fn start_upload(
        &mut self,
        terminal: &ResourceId,
        batch: crate::attach::UploadBatch,
        cx: &mut Context<Self>,
    ) {
        if self.refuse_native_input(terminal) {
            return;
        }
        let Some(client) = self.client() else {
            self.post(
                Kind::Refusal,
                crate::attach::PASTE_REFUSED,
                Some("Not connected. Nothing was pasted.".into()),
            );
            return;
        };
        let Some(started) = self.destination_of(terminal) else {
            self.post(
                Kind::Refusal,
                crate::attach::PASTE_REFUSED,
                Some("That terminal is gone. Nothing was pasted.".into()),
            );
            return;
        };
        if matches!(started.host, crate::attach::TerminalHost::Local) {
            if let crate::attach::UploadBatch::Paths(paths) = batch {
                self.paste_text_to(terminal, &crate::attach::quote_paths(&paths));
            }
            return;
        }
        let epoch = self.upload_epoch;
        self.uploads_reading = self.uploads_reading.saturating_add(1);
        cx.spawn(async move |this, cx| {
            let prepared = smol::unblock(move || crate::attach::prepare_upload(batch)).await;
            let _ = this.update(cx, |desk, cx| {
                desk.uploads_reading = desk.uploads_reading.saturating_sub(1);
                desk.commit_uploads(epoch, &client, &started, prepared);
                cx.notify();
            });
        })
        .detach();
    }

    fn commit_uploads(
        &mut self,
        epoch: u64,
        client: &Arc<Client>,
        started: &crate::attach::UploadDestination,
        prepared: Vec<Result<crate::attach::PreparedUpload, crate::attach::UploadPrepareError>>,
    ) {
        // A replaced connection already explained the abort. A late read stops.
        if self.upload_epoch != epoch {
            return;
        }
        if self
            .client()
            .is_none_or(|current| !Arc::ptr_eq(&current, client))
        {
            self.post(
                Kind::Refusal,
                crate::attach::PASTE_REFUSED,
                Some("The terminal changed before the upload started. Nothing was pasted.".into()),
            );
            return;
        }
        if self.refuse_native_input(&started.terminal) {
            return;
        }
        let current = self.destination_of(&started.terminal);
        if !crate::attach::same_upload_destination(started, current.as_ref()) {
            self.post(
                Kind::Refusal,
                crate::attach::PASTE_REFUSED,
                Some("The terminal changed before the upload started. Nothing was pasted.".into()),
            );
            for item in &prepared {
                if let Err(error) = item {
                    self.post(Kind::Refusal, error.title, Some(error.body.clone()));
                }
            }
            return;
        }
        for item in prepared {
            match item {
                Err(error) => self.post(Kind::Refusal, error.title, Some(error.body)),
                Ok(file) => {
                    let transfer_id =
                        client.put_file(started.terminal.clone(), file.extension, file.bytes);
                    self.uploads.push(PendingUpload {
                        transfer_id,
                        destination: started.clone(),
                        local_source: file.local_source,
                    });
                }
            }
        }
    }

    fn finish_uploads(&mut self, receipts: Vec<FileUploadReceipt>) {
        for receipt in receipts {
            let Some(index) = self
                .uploads
                .iter()
                .position(|pending| pending.transfer_id == receipt.transfer_id)
            else {
                continue;
            };
            let pending = self.uploads.remove(index);
            let current = self.destination_of(&pending.destination.terminal);
            match crate::attach::follow_upload(
                &pending.destination,
                current.as_ref(),
                &receipt,
                pending.local_source.as_deref(),
            ) {
                crate::attach::UploadFollowup::Paste { terminal, text } => {
                    let more = self
                        .uploads
                        .iter()
                        .any(|pending| pending.destination.terminal == terminal);
                    let text = if more { format!("{text} ") } else { text };
                    self.paste_uploaded(&terminal, &text);
                }
                crate::attach::UploadFollowup::Notice { title, body } => {
                    self.post(Kind::Refusal, title, Some(body));
                }
            }
        }
    }

    /// Paste a server path. `input_ready` here means the terminal can take
    /// keystrokes, not that its files are on this machine.
    fn paste_uploaded(&mut self, terminal: &ResourceId, text: &str) {
        if self.refuse_native_input(terminal) {
            return;
        }
        let Some(client) = self.client() else {
            self.post(
                Kind::Refusal,
                crate::attach::PASTE_REFUSED,
                Some(format!(
                    "The file is on that machine at {text}. Nothing was pasted."
                )),
            );
            return;
        };
        let queued = client.with_control(|control| {
            if crate::input::native_owned(control, terminal) || !control.input_ready(terminal) {
                return false;
            }
            let _ = control.apply_paste(terminal, text);
            true
        });
        if queued {
            client.wake();
            return;
        }
        self.post(
            Kind::Refusal,
            crate::attach::PASTE_REFUSED,
            Some(format!(
                "The file is on that machine at {text}. The terminal was not ready, so nothing was pasted."
            )),
        );
    }

    /// The placed terminal and the host [`crate::attach::terminal_host`] names.
    /// Absent when that terminal is no longer on screen or belongs to native ACP.
    fn destination_of(&self, terminal: &ResourceId) -> Option<crate::attach::UploadDestination> {
        if self.native_owned_terminal(terminal) {
            return None;
        }
        let link = self.link.as_ref()?;
        let placed = link.workspace.tabs.iter().any(|tab| {
            crate::workspace::layout::placements(&tab.root)
                .iter()
                .any(|placement| placement.terminal_id.as_ref() == Some(terminal))
        });
        if !placed {
            return None;
        }
        Some(crate::attach::UploadDestination {
            terminal: terminal.clone(),
            host: self.terminal_host(terminal),
        })
    }

    fn abort_uploads(&mut self, notify: bool) {
        let pending = std::mem::take(&mut self.uploads);
        let reading = self.uploads_reading > 0;
        self.upload_epoch = self.upload_epoch.wrapping_add(1);
        if notify && (!pending.is_empty() || reading) {
            self.post(
                Kind::Refusal,
                crate::attach::PASTE_REFUSED,
                Some("The terminal changed before the upload finished. Nothing was pasted.".into()),
            );
        }
    }

    fn write_file(
        &mut self,
        region: &str,
        mode: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.focused_view() else {
            return;
        };
        let Some(client) = self.client() else { return };
        let text = match region {
            "selection" => self.selection_text().unwrap_or_default(),
            "scrollback" => region_text(&client, view, TextRegion::History),
            _ => region_text(&client, view, TextRegion::Screen),
        };
        if text.is_empty() {
            self.post(
                Kind::Refusal,
                if region == "selection" {
                    "Nothing selected"
                } else {
                    "Nothing to write"
                },
                None,
            );
            return;
        }
        let name = match region {
            "selection" => "selection.txt",
            "scrollback" => "scrollback.txt",
            _ => "screen.txt",
        };
        let path = match host::write_temp_file(name, &text) {
            Ok(path) => path,
            Err(error) => {
                self.post(Kind::Error, "Could not write the file", Some(error));
                return;
            }
        };
        let shown = path.display().to_string();
        match mode {
            "paste" => {
                let Some(terminal) = self.focused_terminal() else {
                    return;
                };
                self.paste_host_paths(&terminal, &[path], cx);
            }
            "copy" => self.host_action(HostAction::CopyText(shown), cx),
            _ => self.host_action(HostAction::Open(shown), cx),
        }
    }

    /// Open find on the focused pane with its field focused; reopening re-runs the search.
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(placement_id) = self
            .focused_placement()
            .map(|placement| placement.id.clone())
        else {
            return;
        };
        if let Some(find) = self
            .find
            .as_ref()
            .filter(|find| find.placement_id == placement_id)
        {
            let (query, case) = (find.query.clone(), find.case_sensitive);
            self.run_search(query, case, cx);
            if self.find_input.is_none() {
                self.find_input = Some(super::overlays::find_input(self, "", window, cx));
            }
            return;
        }
        self.close_find(cx);
        self.find = Some(FindState {
            placement_id,
            query: String::new(),
            case_sensitive: false,
            matches: Vec::new(),
            index: 0,
        });
        self.find_input = Some(super::overlays::find_input(self, "", window, cx));
    }

    /// Search for the first selected line, and show it in the field.
    fn find_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(placement) = self.focused_placement().cloned() else {
            return;
        };
        let selected = self.selection_text().unwrap_or_default();
        let query = selected
            .split(['\n', '\r'])
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(512)
            .collect::<String>();
        if query.is_empty() {
            self.open_find(window, cx);
            return;
        }
        let case = self
            .find
            .as_ref()
            .map(|find| find.case_sensitive)
            .unwrap_or(false);
        if self
            .find
            .as_ref()
            .is_some_and(|find| find.placement_id != placement.id)
        {
            self.close_find(cx);
        }
        self.find = Some(FindState {
            placement_id: placement.id,
            query: query.clone(),
            case_sensitive: case,
            matches: Vec::new(),
            index: 0,
        });
        match self.find_input.as_ref() {
            Some((input, _)) => {
                let input = input.clone();
                input.update(cx, |input, cx| input.set_value(query.clone(), window, cx));
            }
            None => self.find_input = Some(super::overlays::find_input(self, &query, window, cx)),
        }
        self.run_search(query, case, cx);
    }

    pub fn run_search(&mut self, query: String, case_sensitive: bool, cx: &mut Context<Self>) {
        let Some(state) = self.find.clone() else {
            return;
        };
        let Some(placement) = self.placement(&state.placement_id).cloned() else {
            return;
        };
        let Some(view) = placement.view_id else {
            return;
        };
        let matches = if query.is_empty() {
            Vec::new()
        } else {
            self.client()
                .and_then(|client| client.engine())
                .and_then(|engine| engine.search_view(view, query.clone(), case_sensitive).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|found| (found.start, found.end))
                .collect()
        };
        self.show_match(&placement, &matches, 0, cx);
        self.find = Some(FindState {
            placement_id: state.placement_id,
            query,
            case_sensitive,
            matches,
            index: 0,
        });
    }

    /// Next or previous match, wrapping. A step with no matches searches again.
    pub(crate) fn step_find(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            self.open_find(window, cx);
            return;
        }
        let Some(state) = self.find.clone() else {
            return;
        };
        if state.matches.is_empty() {
            self.run_search(state.query, state.case_sensitive, cx);
            return;
        }
        let len = state.matches.len() as i32;
        let index = (state.index as i32 + delta).rem_euclid(len) as usize;
        if let Some(placement) = self.placement(&state.placement_id).cloned() {
            self.show_match(&placement, &state.matches, index, cx);
        }
        self.find = Some(FindState { index, ..state });
    }

    fn show_match(
        &self,
        placement: &Placement,
        matches: &[(u64, u64)],
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = placement.view_id else {
            return;
        };
        let Some(client) = self.client() else { return };
        let Some(engine) = client.engine() else {
            return;
        };
        if let Some((start, end)) = matches.get(index) {
            let _ = engine.set_view_selection(view, *start, *end, false);
            let _ = client.pin_viewport_view(view, *start);
        } else {
            let _ = engine.clear_view_selection(view);
        }
        self.repaint_placement(&placement.id, cx);
    }

    fn close_find(&mut self, cx: &mut Context<Self>) {
        self.find_input = None;
        if let Some(state) = self.find.take()
            && let Some(placement) = self.placement(&state.placement_id).cloned()
            && let Some(view) = placement.view_id
            && let Some(engine) = self.client().and_then(|client| client.engine())
        {
            let _ = engine.clear_view_selection(view);
            self.repaint_placement(&placement.id, cx);
        }
    }

    pub fn find_status(&self) -> String {
        let Some(state) = &self.find else {
            return String::new();
        };
        if state.query.is_empty() {
            return String::new();
        }
        if state.matches.is_empty() {
            return "no matches".into();
        }
        format!(
            "{} of {}{}",
            state.index + 1,
            state.matches.len(),
            if state.matches.len() >= 4096 { "+" } else { "" }
        )
    }

    fn open_path_picker(&mut self) {
        let Some(target) = self.path_target() else {
            self.post(
                Kind::Refusal,
                "Insert Path is unavailable",
                Some("Not attached to a terminal.".into()),
            );
            return;
        };
        let features = self
            .link
            .as_ref()
            .map(|link| link.connection.features())
            .unwrap_or_default();
        let attached = self
            .link
            .as_ref()
            .is_some_and(|link| link.connection.status == Status::Attached);
        if !paths::can_offer_path_picker(Some(features.as_slice()), attached, Some(&target)) {
            let body = if attached {
                "This server can't search paths. Update phux on that machine."
            } else {
                "Not attached to a terminal."
            };
            self.post(
                Kind::Refusal,
                "Insert Path is unavailable",
                Some(body.into()),
            );
            return;
        }
        let root = self
            .pane_cwd(crate::connection::parse_terminal(&target.terminal_id).as_ref())
            .unwrap_or_else(|| "~".into());
        self.picker = Some(paths::begin_picker(target, root.clone()));
        self.modal = Modal::Path;
        self.query_paths(root, String::new());
    }

    pub fn query_paths(&mut self, root: String, query: String) {
        let Some(active) = self.picker.clone() else {
            return;
        };
        let Some(current) = self.path_target() else {
            self.close_path_picker();
            return;
        };
        if !paths::same_target(&active.target, Some(&current)) {
            self.close_path_picker();
            return;
        }
        let host = paths::host_for_terminal(&current.terminal_id).map(SatelliteHost::new);
        let id = self
            .link
            .as_ref()
            .map(|link| {
                link.connection
                    .path_query(root.clone(), query.clone(), !query.is_empty(), host)
            })
            .unwrap_or(0);
        self.picker = Some(if id == 0 {
            let mut failed = active;
            failed.root = root;
            failed.query = query;
            failed.pending = None;
            failed.rows.clear();
            failed.message = "Path search is unavailable on this server.".into();
            failed
        } else {
            paths::start_query(active, id, root, &query)
        });
    }

    /// Paste the quoted path into the picker's pane. The picker stays open with
    /// the reason when nothing was queued.
    pub fn insert_path(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.picker.clone() else {
            return;
        };
        let current = self.path_target();
        let ready = current
            .as_ref()
            .and_then(|target| crate::connection::parse_terminal(&target.terminal_id))
            .is_some_and(|id| {
                self.link
                    .as_ref()
                    .is_some_and(|link| link.connection.ready(&id))
            });
        let Some(text) = paths::prepared_insertion(Some(&active), &path, current.as_ref(), ready)
        else {
            let mut rejected = active;
            rejected.message = "The pane changed or is not ready; nothing was inserted.".into();
            self.picker = Some(rejected);
            cx.notify();
            return;
        };
        if self.paste_text(&text) {
            self.close_modal(window, cx);
            self.after_command(window, cx);
        } else if let Some(picker) = self.picker.as_mut() {
            picker.message = "The path was not queued; nothing was inserted.".into();
            cx.notify();
        }
    }

    pub fn open_path_terminal(
        &mut self,
        directory: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_call(|link| {
            link.workspace
                .new_terminal(&link.connection, Some(directory.as_str()))
        });
        self.close_modal(window, cx);
        self.after_command(window, cx);
    }

    /// Browse into a directory: it becomes the root and the query clears.
    pub(crate) fn browse_path(
        &mut self,
        directory: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.palette_state.clone() {
            state.update(cx, |state, cx| state.set_query("", window, cx));
        }
        self.query_paths(directory, String::new());
        cx.notify();
    }

    /// Tab or Right on a directory row (or the parent row) browses into it.
    pub(crate) fn open_selected_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.palette_state.clone() else {
            return;
        };
        let Some(row) = state.read(cx).selected_index().map(|index| index.row) else {
            return;
        };
        let directory = super::overlays::path_items(self)
            .into_iter()
            .nth(row)
            .and_then(|item| item.directory());
        if let Some(directory) = directory {
            self.browse_path(directory, window, cx);
        }
    }

    fn close_path_picker(&mut self) {
        self.picker = None;
        if matches!(self.modal, Modal::Path) {
            self.modal = Modal::None;
        }
    }

    fn pin_picker(&mut self) {
        let Some(active) = self.picker.clone() else {
            return;
        };
        let attached = self
            .link
            .as_ref()
            .is_some_and(|link| link.connection.status == Status::Attached);
        if !attached || !paths::same_target(&active.target, self.path_target().as_ref()) {
            self.close_path_picker();
        }
    }

    fn accept_path(&mut self, answer: PathAnswer) {
        self.picker = paths::accept_path_answer(self.picker.take(), &answer);
    }

    fn path_target(&self) -> Option<paths::PathTarget> {
        let placement = self.focused_placement()?;
        let link = self.link.as_ref()?;
        let server = link.connection.server_id()?;
        Some(paths::PathTarget {
            terminal_id: placement.terminal_label.clone(),
            placement_id: placement.id.clone(),
            server_id: server,
            epoch: link.connection.connection_epoch().to_string(),
        })
    }

    pub fn terminal_settings(
        &self,
        placement: &Placement,
        focused: bool,
    ) -> Option<TerminalSettings> {
        placement.terminal_id.as_ref()?;
        let mut settings = self.look().terminal.clone();
        settings.focused = focused;
        settings.size_owner = self
            .link
            .as_ref()
            .is_some_and(|link| link.workspace.size_owner(placement));
        Some(settings)
    }

    /// Paint values for the current preferences, rebuilt only after they change.
    pub(crate) fn look(&self) -> Rc<Look> {
        let mut cached = self.look.borrow_mut();
        match cached.as_ref() {
            Some(look) if look.prefs == self.prefs => look.clone(),
            _ => {
                let look = Rc::new(Look::new(&self.prefs, &self.ghostty, &self.keymap));
                *cached = Some(look.clone());
                look
            }
        }
    }

    pub fn palette(&self) -> crate::settings::theme::Palette {
        self.look().palette.clone()
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        self.link
            .as_ref()
            .map(|link| link.connection.client.clone())
    }

    /// Change the focused view locally, then repaint it.
    fn with_view(
        &self,
        cx: &mut Context<Self>,
        call: impl FnOnce(&Client, phux_client_runtime::ViewId),
    ) {
        let Some(view) = self.focused_view() else {
            return;
        };
        let Some(client) = self.client() else { return };
        call(&client, view);
        if let Some(id) = self
            .focused_placement()
            .map(|placement| placement.id.clone())
        {
            self.repaint_placement(&id, cx);
        }
    }

    /// Repaint one pane's cached terminal after a local view change, which
    /// republishes without a runtime wake.
    fn repaint_placement(&self, placement_id: &str, cx: &mut Context<Self>) {
        if let Some(entity) = self.panes.get(placement_id) {
            entity.update(cx, |_, cx| cx.notify());
        }
    }

    fn with_engine(
        &self,
        cx: &mut Context<Self>,
        call: impl FnOnce(&phux_client_runtime::engine::EngineHandle, phux_client_runtime::ViewId),
    ) {
        self.with_view(cx, |client, view| {
            if let Some(engine) = client.engine() {
                call(&engine, view);
            }
        });
    }

    fn selection_text(&self) -> Option<String> {
        let view = self.focused_view()?;
        let client = self.client()?;
        let bytes = client.engine()?.view_selection_text(view).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub(crate) fn focused_view(&self) -> Option<phux_client_runtime::ViewId> {
        self.focused_placement()?.view_id
    }

    pub(crate) fn focused_terminal(&self) -> Option<ResourceId> {
        self.focused_placement()?.terminal_id.clone()
    }

    pub fn focused_placement(&self) -> Option<&Placement> {
        self.link.as_ref().and_then(|link| link.workspace.focused())
    }

    pub fn placement(&self, id: &str) -> Option<&Placement> {
        let link = self.link.as_ref()?;
        link.workspace.tabs.iter().find_map(|tab| {
            crate::workspace::layout::placements(&tab.root)
                .into_iter()
                .find(|placement| placement.id == id)
        })
    }

    pub(crate) fn active_tab(&self) -> Option<&crate::workspace::layout::DeskTab> {
        self.link
            .as_ref()
            .and_then(|link| link.workspace.active_tab())
    }

    pub(crate) fn active_tab_id(&self) -> Option<String> {
        self.active_tab().map(|tab| tab.id.clone())
    }

    /// The terminal's label by [`pane_label`].
    pub fn pane_title(&self, terminal: Option<&ResourceId>) -> String {
        let Some((link, terminal)) = self.link.as_ref().zip(terminal) else {
            return pane_label(None, None);
        };
        pane_label(
            link.connection.pane(terminal),
            link.connection.group_name(terminal),
        )
    }

    /// Human connection status and its tone, for the status bar and Settings.
    pub(crate) fn connection_label(&self) -> (String, Tone) {
        let Some(link) = self.link.as_ref() else {
            return ("Not connected".into(), Tone::Bad);
        };
        match link.connection.status {
            Status::Attached => ("Connected".into(), Tone::Good),
            Status::Idle | Status::Connecting | Status::Negotiated => {
                ("Connecting…".into(), Tone::Busy)
            }
            Status::Failed => ("Not connected".into(), Tone::Bad),
            Status::Closed => ("Disconnected".into(), Tone::Bad),
        }
    }

    /// The session is attached, so a terminal reacquire can paint.
    pub(crate) fn session_is_attached(&self) -> bool {
        self.link
            .as_ref()
            .is_some_and(|link| link.connection.status == Status::Attached)
    }

    /// Whether the connection needs the user to reconnect.
    pub(crate) fn unhealthy(&self) -> bool {
        self.link
            .as_ref()
            .is_none_or(|link| link.connection.status.is_terminal())
    }

    /// Open a modal from a pointer action: its controls take the keyboard.
    pub(crate) fn open_modal(&mut self, modal: Modal, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal != Modal::None {
            self.close_modal(window, cx);
        }
        self.modal = modal;
        self.after_command(window, cx);
    }

    pub(crate) fn toggle_session_collapsed(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(name) {
            self.collapsed.insert(name.to_owned());
        }
        cx.notify();
    }

    fn pane_cwd(&self, terminal: Option<&ResourceId>) -> Option<String> {
        let terminal = terminal?;
        self.link
            .as_ref()
            .and_then(|link| link.connection.pane(terminal))
            .and_then(|pane| pane.cwd.clone())
    }

    pub(crate) fn focused_pane(&self) -> Option<&phux_client_runtime::control::PaneDescriptor> {
        let terminal = self.focused_terminal()?;
        self.link
            .as_ref()
            .and_then(|link| link.connection.pane(&terminal))
    }

    pub fn agent(&self, terminal: &ResourceId) -> Option<&AgentInfo> {
        self.link
            .as_ref()
            .and_then(|link| link.connection.agents.get(terminal))
    }

    pub fn post(&mut self, kind: Kind, title: &str, body: Option<String>) {
        self.feed.post(kind, title, body);
    }

    /// Reveal a notification's terminal, if it has one, and mark it read.
    pub fn open_notification(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = self.feed.get(id).and_then(|entry| entry.terminal.clone());
        self.feed.read(id);
        if self.modal == Modal::Notifications {
            self.close_modal(window, cx);
        }
        if let Some(terminal) = terminal {
            self.reveal_terminal(&terminal);
        }
        self.after_command(window, cx);
    }

    /// Mirror new agent and error entries to the system notification center
    /// while no Phux window is active. A click comes back through
    /// [`super::session::Desktop`] by the tag's desk entity and entry id.
    pub(crate) fn flush_system(&mut self, cx: &mut Context<Self>) {
        let outbound = self.feed.take_outbound();
        if outbound.is_empty() || cx.active_window().is_some() {
            return;
        }
        let desk = cx.entity_id().as_u64();
        for entry in outbound {
            cx.show_system_notification(gpui::SystemNotification {
                tag: format!("{desk}/{}", entry.id).into(),
                title: entry.title.into(),
                body: entry.body.unwrap_or_default().into(),
                actions: Vec::new(),
            });
        }
    }

    /// Expire the peek card and keep agent ages current. Armed from every
    /// render; it wakes when the next label changes and ends when nothing
    /// needs it.
    pub(crate) fn ensure_clock(&mut self, cx: &mut Context<Self>) {
        let Some(wait) = self.clock_wait() else {
            return;
        };
        let at = Instant::now() + wait;
        if self._clock.is_some() && self.clock_at.is_some_and(|armed| armed <= at) {
            return;
        }
        self.clock_at = Some(at);
        self._clock = Some(cx.spawn(async move |this, cx| {
            let mut wait = wait;
            loop {
                smol::Timer::after(wait).await;
                let next = this
                    .update(cx, |desk, cx| {
                        desk.feed.expire_peek(Instant::now());
                        cx.notify();
                        let next = desk.clock_wait();
                        desk.clock_at = next.map(|wait| Instant::now() + wait);
                        if next.is_none() {
                            desk._clock = None;
                        }
                        next
                    })
                    .ok()
                    .flatten();
                let Some(next) = next else {
                    break;
                };
                wait = next;
            }
        }));
    }

    /// How long until the peek card expires or an agent's age label changes.
    pub(crate) fn clock_wait(&self) -> Option<Duration> {
        let peek = self
            .feed
            .peek_expires()
            .map(|at| at.saturating_duration_since(Instant::now()));
        let now = unix_ms();
        let ages = self.link.iter().flat_map(|link| {
            link.connection
                .agents
                .values()
                .map(move |info| super::view::ago_wait(info.changed_at_ms, now))
        });
        peek.into_iter()
            .chain(ages.min().map(Duration::from_millis))
            .min()
    }

    fn retain_panes(&mut self) {
        let live = self
            .link
            .as_ref()
            .map(|link| {
                link.workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| crate::workspace::layout::placements(&tab.root))
                    .map(|placement| placement.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.panes.retain(&live);
        let connection = self.link.as_ref().map(|link| &link.connection);
        self.conversation_faces.retain(|placement, face| {
            face.retain_for_placement(live.contains(placement), connection)
        });
    }

    pub fn panes(&self) -> &Panes {
        &self.panes
    }

    pub fn panes_mut(&mut self) -> &mut Panes {
        &mut self.panes
    }

    /// Re-read the remote registry on the background executor, which a test
    /// scheduler drives like any other window-open work.
    pub(crate) fn refresh_hosts(&mut self, cx: &mut Context<Self>) {
        let phux = self.phux.clone();
        let listed = cx
            .background_executor()
            .spawn(async move { host::list_hosts(&phux) });
        cx.spawn(async move |desk, cx| {
            let result = listed.await;
            let _ = desk.update(cx, |desk, cx| {
                match result {
                    Ok(hosts) => {
                        desk.hosts = hosts;
                        desk.sync_fleet(cx);
                    }
                    // No CLI is already explained at launch; the section just stays empty.
                    Err(error) => log::warn!("host list: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Point this window at another host in place: `None` is this machine,
    /// `Some(name)` a registered remote. The current connection and views are
    /// released and the target's home session and saved layout take their
    /// place. No new window is opened.
    #[cfg(feature = "ui-fixtures")]
    pub(crate) fn switch_host(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        if self.host_switching || self.remote == name {
            return;
        }
        self.host_switching = true;
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let target = name.clone();
        cx.spawn(async move |desk, cx| {
            let probe = target.clone();
            let resolved = smol::unblock(move || match probe.as_deref() {
                Some(host) => crate::connection::home::open_remote_home(host),
                None => crate::connection::home::open_home(&phux, &socket, None),
            })
            .await;
            let _ = desk.update(cx, |desk, cx| {
                desk.host_switching = false;
                match resolved {
                    Ok(session) => desk.adopt_host(target, session, cx),
                    Err(error) => {
                        desk.post(Kind::Error, "Could not switch host", Some(error));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// The in-place half of [`Self::switch_host`], once the target's home is known.
    #[cfg(feature = "ui-fixtures")]
    fn adopt_host(&mut self, name: Option<String>, session: String, cx: &mut Context<Self>) {
        self.adopt_work(name, session, None, None, cx);
    }

    fn work_layout_path(
        &self,
        host: Option<&str>,
        session: &str,
        key: Option<SessionKey>,
    ) -> PathBuf {
        let authority = host.map_or_else(
            || self.socket.display().to_string(),
            |host| format!("host:{host}"),
        );
        if let Some(key) = key {
            let paths = persist::session_layout_paths(&authority, key);
            return if self.role == WindowRole::Quick {
                paths.quick
            } else {
                paths.layout
            };
        }
        let paths = crate::workspace::layout_paths(&authority, session);
        if self.role == WindowRole::Quick {
            paths.quick
        } else {
            paths.layout
        }
    }

    pub(super) fn adopt_work(
        &mut self,
        name: Option<String>,
        session: String,
        key: Option<SessionKey>,
        authority: Option<phux_protocol::ids::AuthorityId>,
        cx: &mut Context<Self>,
    ) {
        let previous = self.work_layout_path(
            self.remote.as_deref(),
            &self.session,
            self.link
                .as_ref()
                .and_then(|link| link.connection.home_key())
                .or(self.home_key),
        );
        self.locations.insert(previous, self.store.clone());
        self.persist();
        self.abandon_organize();
        self.abort_uploads(true);
        if let Some(link) = self.link.as_mut() {
            link.workspace.release_all(&link.connection);
            link.connection.close();
        }
        self.link = None;
        self.panes.clear();
        self.agent_seen.clear();
        self.feed.retain(|entry| entry.kind != Kind::Agent);
        self.find = None;
        self.find_input = None;
        self.pending_project_terminal = None;
        self.connect_error = None;
        self.goto_search = Default::default();
        self.remote = name.clone();
        self.session = session.clone();
        if self.role == WindowRole::Fresh {
            self.role = WindowRole::Main;
        }
        let id = cx.entity_id();
        self.desktop.update(cx, |desktop, _| {
            desktop.sync_window_session(id, &session, name.as_deref(), self.role)
        });
        self.home_key = key;
        self.expected_authority = authority;
        self.modal = Modal::None;
        self.prepared = Modal::None;
        self.modal_inputs.clear();
        self.picker = None;
        self.organization = Default::default();
        self.creates.clear();
        self.renames.clear();
        self.kills.clear();
        self.layout_key = None;
        self.last_saved = None;
        self.open_fresh = false;
        let path = self.work_layout_path(name.as_deref(), &session, key);
        // In particular A -> B -> A must see A's latest queued layout. Disk
        // waits are confined to this read-after-write boundary and close/quit.
        self.flush_saves();
        self.store = self
            .locations
            .get(&path)
            .cloned()
            .unwrap_or_else(|| FileLayoutStore::new(path, None));
        let loaded = persist::load_layout(&self.store.read());
        self.layout_blocked = loaded.blocked;
        self.persist_writes = self.role != WindowRole::Fresh && !loaded.blocked;
        self.layout_migration_pending = false;
        self.last_saved = loaded.initial;
        self.attach(None, cx);
        self.sync_fleet(cx);
        self.sync_bindings(cx);
        self.apply_kit_theme(cx);
        cx.notify();
    }

    /// Enroll `target` over ssh. The dialog closes at once; enrollment runs in
    /// the background and reports through a notification.
    pub(crate) fn apply_add_host(
        &mut self,
        target: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = target.trim().to_owned();
        if target.is_empty() {
            self.post(
                Kind::Refusal,
                "Could not add host",
                Some("Enter an ssh destination such as me@mini.".into()),
            );
            cx.notify();
            return;
        }
        if self.host_adding.is_some() {
            return;
        }
        self.close_modal(window, cx);
        self.host_adding = Some(target.clone());
        let phux = self.phux.clone();
        cx.spawn(async move |desk, cx| {
            let added = target.clone();
            let result = smol::unblock(move || host::add_host(&phux, &target, &name)).await;
            let _ = desk.update(cx, |desk, cx| {
                desk.host_adding = None;
                match result {
                    Ok(()) => desk.post(Kind::Info, &format!("Added {added}"), None),
                    Err(error) => {
                        desk.post(Kind::Error, &format!("Could not add {added}"), Some(error))
                    }
                }
                desk.refresh_hosts(cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn confirm_remove_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Modal::RemoveHost { name } = self.modal.clone() else {
            return;
        };
        self.close_modal(window, cx);
        match host::remove_host(&self.phux, &name) {
            Ok(()) => self.hosts.retain(|host| host.name != name),
            Err(error) => self.post(
                Kind::Error,
                &format!("Could not remove {name}"),
                Some(error),
            ),
        }
        self.refresh_hosts(cx);
    }

    /// Why a CLI-backed action cannot run here: the CLI drives the local
    /// server's socket, not a registered host.
    fn local_only(&self) -> Option<String> {
        self.remote.as_deref().map(|host| {
            format!("This window is on {host}. Run it there with `phux attach {host}`.")
        })
    }

    /// `{tab} — phux`, or `phux` with no tab, plus the host on a remote window.
    /// Written only when it changes.
    pub(crate) fn sync_title(&mut self, window: &mut Window) {
        let title = self
            .active_tab()
            .map(|tab| format!("{} — phux", tab_title(self, tab)))
            .unwrap_or_else(|| "phux".into());
        let title = match self.remote.as_deref() {
            Some(host) => format!("{title} · {host}"),
            None => title,
        };
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
    }

    /// Give the platform window the background the preferences ask for. A
    /// solid window is opaque all the way down: the window, its Metal layer
    /// and the painted sheet, so the compositor neither blurs nor blends it.
    pub(crate) fn sync_window_background(&mut self, window: &mut Window) {
        let wanted = self.prefs.window_background;
        if wanted != self.window_background {
            window.set_background_appearance(window_options_background(wanted));
            self.window_background = wanted;
        }
    }

    /// Reconcile the keyboard after state changed without a window at hand (a
    /// modal or find bar closed by server activity): nothing focused falls back
    /// to the terminal, or to the window so its key bindings keep working.
    pub(crate) fn settle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal == Modal::None && self.prepared != Modal::None {
            self.prepared = Modal::None;
            self.modal_inputs.clear();
            self.focus_terminal(window, cx);
        } else if self.modal == Modal::None && window.focused(cx).is_none() {
            self.focus_terminal(window, cx);
        }
    }

    pub(crate) fn dragging_split(&self) -> Option<&str> {
        match self.drag.as_ref()? {
            Drag::Split { id, .. } => Some(id.as_str()),
            _ => None,
        }
    }

    pub(crate) fn dragging_sidebar(&self) -> bool {
        matches!(self.drag, Some(Drag::Sidebar))
    }

    pub fn set_viewport(&mut self, size: gpui::Size<gpui::Pixels>) {
        self.viewport = size;
    }

    pub fn frame_report(&self, cx: &gpui::App) -> crate::diag::FrameReport {
        let mut placements = Vec::new();
        let mut painted = Vec::new();
        if let Some(link) = &self.link
            && let Some(tab) = link
                .workspace
                .tabs
                .iter()
                .find(|tab| tab.id == link.workspace.active_id)
        {
            for placement in crate::workspace::layout::placements(&tab.root) {
                placements.push((placement.id.clone(), placement.terminal_label.clone()));
                let Some(entity) = self.panes.get(&placement.id) else {
                    continue;
                };
                let observation = entity.read(cx).painted();
                let bounds = observation.geometry.bounds;
                painted.push(crate::diag::PaintedPane {
                    placement_id: placement.id.clone(),
                    terminal_id: placement.terminal_label.clone(),
                    generation: observation
                        .frame
                        .as_ref()
                        .map(|frame| frame.generation)
                        .unwrap_or(0),
                    bounds,
                    unclipped: pane_unclipped(self.viewport, bounds),
                });
            }
        }
        crate::diag::FrameReport {
            session: self.session.clone(),
            placements,
            painted,
        }
    }

    pub fn begin_sidebar_drag(&mut self, cx: &mut Context<Self>) {
        self.drag = Some(Drag::Sidebar);
        cx.notify();
    }

    pub fn begin_split_drag(&mut self, id: String, axis: Axis, cx: &mut Context<Self>) {
        self.drag = Some(Drag::Split { id, axis });
        cx.notify();
    }

    /// Quick windows keep their compact titlebar regardless of the main layout.
    pub(crate) fn tab_position(&self) -> TabPosition {
        if self.role == WindowRole::Quick {
            TabPosition::Top
        } else {
            self.prefs.tab_position
        }
    }

    pub fn begin_tab_drag(&mut self, id: String, x: f32, y: f32, cx: &mut Context<Self>) {
        let position = self.tab_position();
        self.drag = Some(Drag::Tab {
            id,
            position,
            start: tab_coordinate(position, x, y),
            moved: false,
        });
        cx.notify();
    }

    /// The window-wide cursor while a drag is armed.
    pub(crate) fn drag_cursor(&self) -> Option<CursorStyle> {
        match self.drag.as_ref()? {
            Drag::Sidebar
            | Drag::Split {
                axis: Axis::Row, ..
            } => Some(CursorStyle::ResizeLeftRight),
            Drag::Split {
                axis: Axis::Column, ..
            } => Some(CursorStyle::ResizeUpDown),
            Drag::Tab { moved: true, .. } => Some(CursorStyle::ClosedHand),
            Drag::Tab { .. } => Some(CursorStyle::Arrow),
        }
    }

    /// A window-level pointer move while a drag is armed. A move with no
    /// button held means the release was missed, and ends the drag.
    pub fn drag_move(&mut self, x: f32, y: f32, pressed: bool, cx: &mut Context<Self>) {
        if !pressed {
            self.end_drag(cx);
            return;
        }
        match self.drag.as_mut() {
            Some(Drag::Sidebar) => {
                let width = f64::from(x)
                    .round()
                    .clamp(crate::settings::SIDEBAR_MIN, crate::settings::SIDEBAR_MAX);
                self.prefs.sidebar_width = width;
                cx.notify();
            }
            Some(Drag::Split { id, axis }) => {
                let id = id.clone();
                let axis = *axis;
                let Some(bounds) = self.split_bounds.borrow().get(&id).copied() else {
                    return;
                };
                let ratio = match axis {
                    Axis::Row => {
                        (x - bounds.origin.x.as_f32()) / bounds.size.width.as_f32().max(1.)
                    }
                    Axis::Column => {
                        (y - bounds.origin.y.as_f32()) / bounds.size.height.as_f32().max(1.)
                    }
                };
                self.workspace_mut(|link| link.workspace.resize_split(&id, ratio));
                cx.notify();
            }
            Some(Drag::Tab {
                id,
                position,
                start,
                moved,
            }) => {
                let coordinate = tab_coordinate(*position, x, y);
                if !*moved && (coordinate - *start).abs() < 6. {
                    return;
                }
                *moved = true;
                let id = id.clone();
                let from = self
                    .link
                    .as_ref()
                    .and_then(|link| link.workspace.tabs.iter().position(|tab| tab.id == id));
                let Some(from) = from else { return };
                let to = self.tab_geometry.borrow().drop_index(from, *position, x, y);
                if to != from {
                    self.workspace_mut(|link| link.workspace.move_tab(from, to));
                }
                cx.notify();
            }
            None => {}
        }
    }

    /// Release ends every drag and saves what it changed.
    pub fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.persist();
            cx.notify();
        }
    }

    pub fn new_terminal_in(
        &mut self,
        session_id: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_call(|link| {
            let cwd = link
                .connection
                .directories
                .get(&session_id)
                .map(String::as_str);
            link.workspace
                .new_terminal_in(&link.connection, session_id, cwd)
        });
        self.after_command(window, cx);
    }

    /// A terminal reports a click: focus its placement.
    pub(crate) fn on_terminal_event(
        &mut self,
        placement_id: &str,
        event: &crate::terminal::TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            crate::terminal::TerminalEvent::Activated => {
                if let Some(tab) = self.active_tab_id() {
                    self.focus_placement(&tab, placement_id, window, cx);
                }
            }
            crate::terminal::TerminalEvent::Notice { title, body } => {
                self.post(Kind::Refusal, title, Some(body.clone()));
            }
            crate::terminal::TerminalEvent::Upload(batch) => {
                let Some(terminal) = self
                    .placement(placement_id)
                    .and_then(|placement| placement.terminal_id.clone())
                else {
                    self.post(
                        Kind::Refusal,
                        crate::attach::PASTE_REFUSED,
                        Some("That terminal is gone. Nothing was pasted.".into()),
                    );
                    return;
                };
                if self.refuse_native_input(&terminal) {
                    cx.notify();
                    return;
                }
                self.start_upload(&terminal, batch.clone(), cx);
            }
        }
        cx.notify();
    }
}

/// Where a dragged tab lands along either axis, excluding its own center.
pub(crate) fn tab_drop_index(from: usize, centers: &[f32], coordinate: f32) -> usize {
    if from >= centers.len() || centers.iter().any(|center| !center.is_finite()) {
        return from;
    }
    let mut to = 0;
    for (index, center) in centers.iter().enumerate() {
        if index != from && coordinate > *center {
            to = if index < from { index + 1 } else { index };
        }
    }
    to
}

/// The platform background for a window preference. Windows open with the
/// default and [`Desk::sync_window_background`] moves them to the preference.
#[must_use]
pub(crate) fn window_options_background(
    background: WindowBackground,
) -> gpui::WindowBackgroundAppearance {
    match background {
        WindowBackground::Blur => gpui::WindowBackgroundAppearance::Blurred,
        WindowBackground::Solid => gpui::WindowBackgroundAppearance::Opaque,
    }
}

#[cfg(test)]
mod tests {
    use super::{TabGeometry, pane_label, tab_drop_index};
    use crate::settings::TabPosition;
    use phux_client_runtime::control::PaneDescriptor;
    use phux_protocol::ResourceId;

    fn pane(name: Option<&str>, title: Option<&str>, cwd: Option<&str>) -> PaneDescriptor {
        PaneDescriptor {
            terminal_id: ResourceId::local(1),
            session_id: 1,
            session_name: "work".into(),
            cols: 80,
            rows: 24,
            title: title.map(str::to_owned),
            cwd: cwd.map(str::to_owned),
            is_focused: false,
            key: None,
            revision: None,
            name: name.map(str::to_owned),
            activation: None,
            exec: None,
        }
    }

    #[test]
    fn a_label_is_the_name_then_title_then_folder_then_group() {
        let group = Some("logs");
        let all = pane(Some("api"), Some("vim"), Some("/src/phux"));
        assert_eq!(pane_label(Some(&all), group), "api");
        let untitled = pane(Some(" "), Some("vim"), Some("/src/phux"));
        assert_eq!(pane_label(Some(&untitled), group), "vim");
        assert_eq!(
            pane_label(Some(&pane(None, None, Some("/src/phux/"))), group),
            "phux"
        );
        assert_eq!(pane_label(Some(&pane(None, None, None)), group), "logs");
        assert_eq!(pane_label(Some(&pane(None, None, None)), None), "shell");
        assert_eq!(pane_label(None, group), "shell");
    }

    #[test]
    fn a_dragged_tab_lands_after_the_last_center_left_of_the_pointer() {
        let centers = [50., 150., 250., 350.];
        // Tab 0 dragged between tabs 2 and 3 lands at index 2.
        assert_eq!(tab_drop_index(0, &centers, 300.), 2);
        // Tab 3 dragged between tabs 0 and 1 lands at index 1.
        assert_eq!(tab_drop_index(3, &centers, 100.), 1);
        // Past every center: the end. Before every center: the front.
        assert_eq!(tab_drop_index(1, &centers, 400.), 3);
        assert_eq!(tab_drop_index(2, &centers, 10.), 0);
        // A small move inside its own slot stays put.
        assert_eq!(tab_drop_index(1, &centers, 160.), 1);
    }

    #[test]
    fn tab_reordering_uses_the_painted_axis_and_rejects_stale_geometry() {
        let mut geometry = TabGeometry::default();
        for position in [TabPosition::Top, TabPosition::Side] {
            geometry.prepare(position, 3);
            // Before the new strip has painted, no drop may reorder the workspace.
            assert_eq!(geometry.drop_index(1, position, 500., 500.), 1);
            for index in 0..3 {
                // Nonzero origins model the titlebar/rail and a scrolled strip.
                let bounds = gpui::Bounds::new(
                    gpui::point(
                        gpui::px(200. + index as f32 * 100.),
                        gpui::px(-40. + index as f32 * 40.),
                    ),
                    gpui::size(gpui::px(80.), gpui::px(32.)),
                );
                geometry.record(index, position, bounds);
            }
            match position {
                TabPosition::Top => {
                    assert_eq!(geometry.drop_index(0, position, 500., -1000.), 2);
                    assert_eq!(geometry.drop_index(2, position, 220., 1000.), 0);
                }
                TabPosition::Side => {
                    assert_eq!(geometry.drop_index(0, position, -1000., 100.), 2);
                    assert_eq!(geometry.drop_index(2, position, 1000., -30.), 0);
                }
            }
            let other = if position == TabPosition::Top {
                TabPosition::Side
            } else {
                TabPosition::Top
            };
            assert_eq!(geometry.drop_index(1, other, 1000., 1000.), 1);
        }
        assert_eq!(tab_drop_index(2, &[], 10.), 2);
    }
}

fn region_text(client: &Client, view: phux_client_runtime::ViewId, region: TextRegion) -> String {
    let Some(engine) = client.engine() else {
        return String::new();
    };
    match engine.view_region_text_bounded(view, region, SEARCH_BYTES) {
        Ok(phux_client_runtime::engine::BoundedSelectionText::Text(bytes)) => {
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => String::new(),
    }
}

pub(crate) fn short_path(path: Option<String>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    if let Some(rest) = path
        .strip_prefix("/Users/")
        .and_then(|rest| rest.split_once('/').map(|(_, rest)| rest))
    {
        return format!("~/{rest}");
    }
    if let Some(rest) = path
        .strip_prefix("/home/")
        .and_then(|rest| rest.split_once('/').map(|(_, rest)| rest))
    {
        return format!("~/{rest}");
    }
    path
}

/// How the status bar colours a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Good,
    Busy,
    Bad,
}

/// One title rule for tabs, the sidebar, headers and the palette: the
/// resource's own name, else its title, else its directory's name, else the
/// name of the arrangement group that shows it, else "shell".
pub(crate) fn pane_label(
    pane: Option<&phux_client_runtime::control::PaneDescriptor>,
    group: Option<&str>,
) -> String {
    let Some(pane) = pane else {
        return "shell".into();
    };
    let named = |text: Option<&str>| {
        text.map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    named(pane.name.as_deref())
        .or_else(|| named(pane.title.as_deref()))
        .or_else(|| {
            pane.cwd
                .as_deref()
                .and_then(|cwd| cwd.rsplit('/').find(|part| !part.is_empty()))
                .map(str::to_owned)
        })
        .or_else(|| named(group))
        .unwrap_or_else(|| "shell".into())
}

pub fn tab_title(desk: &Desk, tab: &crate::workspace::layout::DeskTab) -> String {
    if let Some(title) = tab.title.as_deref().filter(|title| !title.is_empty()) {
        return title.to_owned();
    }
    let focus = crate::workspace::layout::placements(&tab.root)
        .into_iter()
        .find(|placement| placement.id == tab.focused_id);
    desk.pane_title(focus.and_then(|placement| placement.terminal_id.as_ref()))
}

fn pane_unclipped(viewport: gpui::Size<gpui::Pixels>, bounds: gpui::Bounds<gpui::Pixels>) -> bool {
    let right = bounds.origin.x.as_f32() + bounds.size.width.as_f32();
    let bottom = bounds.origin.y.as_f32() + bounds.size.height.as_f32();
    bounds.origin.x.as_f32() >= 0.
        && bounds.origin.y.as_f32() >= 0.
        && right <= viewport.width.as_f32()
        && bottom <= viewport.height.as_f32()
        && bounds.size.width.as_f32() > 0.
        && bounds.size.height.as_f32() > 0.
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

#[cfg(test)]
mod paint_batch_tests {
    use super::paint_batch;
    use phux_client_runtime::control::Event;
    use phux_protocol::ResourceId;

    #[test]
    fn repeated_agent_metadata_rides_the_paint_path() {
        let output = Event::TerminalChanged {
            terminal_id: ResourceId::local(1),
        };
        let metadata = Event::AgentMetadata {
            terminal_id: ResourceId::local(1),
            value: None,
        };
        assert!(paint_batch(std::slice::from_ref(&output), false));
        assert!(paint_batch(&[output.clone(), metadata.clone()], false));
        assert!(!paint_batch(&[output, metadata], true));
        assert!(!paint_batch(&[], false));
        assert!(!paint_batch(&[Event::TopologyChanged], false));
    }
}
