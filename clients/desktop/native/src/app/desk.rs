//! One window: connection, workspace, and the command path shared by keys, menus, and controls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, KeyDownEvent, Subscription, Task, Window, px,
};
use phux_client_runtime::control::{DeliveryOutcome, Event, PathAnswer, Status};
use phux_client_runtime::engine::{Scroll, TextRegion};
use phux_client_runtime::Client;
use phux_protocol::ResourceId;
use phux_protocol::ids::SatelliteHost;

use crate::connection::path::{self as paths};
use crate::connection::{AgentInfo, Connection};
use crate::settings::{self, DisplayPrefs};
use crate::terminal::{HostAction, Settings as TerminalSettings};
use crate::workspace::layout::{Axis, Direction, Placement};
use crate::workspace::persist::{self, FileLayoutStore};
use crate::workspace::{Workspace, WorkspaceOptions};

use super::actions::Run;
use super::host;
use super::keys::{gpui_stroke, shell_chord};
use super::panes::Panes;
use super::session::Desktop;
use super::typed::typed_keys;

const TOAST_LIMIT: usize = 4;
const SEARCH_BYTES: usize = 8 * 1024 * 1024;

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
    pub fresh: bool,
    pub initial_terminal: Option<ResourceId>,
    pub layout_path: PathBuf,
    pub legacy_layout: Option<PathBuf>,
    pub startup_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
    Agent,
    Success,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub title: String,
    pub body: Option<String>,
    pub at: Instant,
    pub terminal: Option<ResourceId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    None,
    Commands,
    Goto,
    Settings,
    Path,
    Rename { tab_id: String },
    Session { create: bool, current: String },
    CloseSession { name: String },
    WindowSession,
    Terminate { terminal: ResourceId, title: String },
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
    pub role: WindowRole,
    pub socket: PathBuf,
    pub session: String,
    pub phux: PathBuf,
    pub focus: FocusHandle,
    pub prefs: DisplayPrefs,
    pub ghostty_found: bool,
    pub modal: Modal,
    pub find: Option<FindState>,
    pub picker: Option<paths::PickerState>,
    pub toasts: Vec<Toast>,
    pub now: Instant,
    pub link: Option<Link>,
    pub connect_error: Option<String>,
    pub layout_blocked: bool,
    pub persist_writes: bool,
    store: FileLayoutStore,
    last_saved: Option<persist::SavedLayout>,
    panes: Panes,
    agent_seen: HashMap<ResourceId, String>,
    agent_epoch: Option<u64>,
    toast_id: u64,
    drag: Option<Drag>,
    content_origin: gpui::Point<gpui::Pixels>,
    content_size: gpui::Size<gpui::Pixels>,
    tab_centers: Vec<(String, f32)>,
    viewport: gpui::Size<gpui::Pixels>,
    _wake: Option<Task<()>>,
    _clock: Option<Task<()>>,
    _subs: Vec<Subscription>,
    last_run: Option<(String, Instant)>,
    pub(crate) inputs: Vec<Option<gpui::Entity<gpui_kit::component::input::InputState>>>,
    subscribed: Vec<String>,
}

pub struct Link {
    pub connection: Connection,
    pub workspace: Workspace,
}

enum Drag {
    Sidebar,
    Split { id: String, axis: Axis },
    Tab { id: String, start_x: f32, moved: bool },
}

impl Desk {
    pub fn open(
        desktop: Entity<Desktop>,
        phux: PathBuf,
        socket: PathBuf,
        request: OpenRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = FileLayoutStore::new(request.layout_path.clone(), request.legacy_layout.clone());
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
            prefs = persist::quick_display(Some(&prefs), saved.as_ref());
        }
        prefs = settings::sanitize_display(&prefs);
        let focus = cx.focus_handle();
        let mut desk = Self {
            desktop,
            role: request.role,
            socket,
            session: request.session.clone(),
            phux,
            focus,
            prefs,
            ghostty_found: text.is_some(),
            modal: Modal::None,
            find: None,
            picker: None,
            toasts: Vec::new(),
            now: Instant::now(),
            link: None,
            connect_error: None,
            layout_blocked: blocked,
            persist_writes: request.role != WindowRole::Fresh && !blocked,
            store,
            last_saved: saved,
            panes: Panes::default(),
            agent_seen: HashMap::new(),
            agent_epoch: None,
            toast_id: 0,
            drag: None,
            content_origin: gpui::point(px(0.), px(40.)),
            content_size: gpui::size(px(800.), px(600.)),
            tab_centers: Vec::new(),
            viewport: gpui::size(px(1320.), px(840.)),
            _wake: None,
            _clock: None,
            _subs: Vec::new(),
            last_run: None,
            inputs: Vec::new(),
            subscribed: Vec::new(),
        };
        if let Some(error) = request.startup_error {
            desk.toast(ToastKind::Error, "Could not start the phux server", Some(error));
        }
        if blocked {
            desk.toast(
                ToastKind::Error,
                "Saved layout preserved",
                Some("This layout is invalid or from an unsupported version. Layout and settings changes will not be saved.".into()),
            );
        }
        desk.attach(request.fresh, request.initial_terminal, cx);
        desk.sync_bindings(cx);
        let _ = window;
        desk
    }

    fn attach(&mut self, fresh: bool, initial: Option<ResourceId>, cx: &mut Context<Self>) {
        match Connection::connect(self.socket.clone(), self.session.clone()) {
            Ok(connection) => {
                let client = connection.client.clone();
                let mut workspace = Workspace::new(
                    client,
                    self.session.clone(),
                    WorkspaceOptions { fresh, initial_terminal: initial },
                );
                if !fresh {
                    workspace.restore(self.last_saved.clone());
                } else if let Some(display) = self.last_saved.as_ref().map(|saved| saved.display.clone()) {
                    let _ = display;
                }
                self.arm_wake(&connection, cx);
                self.link = Some(Link { connection, workspace });
                self.connect_error = None;
            }
            Err(error) => {
                self.connect_error = Some(error.to_string());
                self.toast(ToastKind::Error, "Could not connect", Some(error.to_string()));
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
        let (events, answers, epoch) = {
            let Some(link) = self.link.as_mut() else { return };
            (
                link.connection.drain(),
                link.connection.take_path_answers(),
                link.connection.connection_epoch(),
            )
        };
        if self.agent_epoch != Some(epoch) || events.iter().any(crate::connection::starts_connection) {
            self.agent_seen.clear();
            self.toasts.retain(|toast| toast.kind != ToastKind::Agent);
            self.agent_epoch = Some(epoch);
        }
        for event in &events {
            self.note_event(event);
        }
        if let Some(link) = self.link.as_mut() {
            link.workspace.handle(&events, &link.connection);
            link.workspace.settle(&link.connection);
        }
        self.take_notices();
        self.note_agents();
        for answer in answers {
            self.accept_path(answer);
        }
        self.retain_panes();
        let _ = cx;
        if self.find.as_ref().is_some_and(|find| self.placement(find.placement_id.as_str()).is_none()) {
            self.find = None;
        }
        if self.modal == Modal::Path {
            self.pin_picker();
        }
        self.ensure_clock(cx);
        cx.notify();
    }

    fn note_event(&mut self, event: &Event) {
        if let Event::TerminalClosed { terminal_id, exit_status, signal, .. } = event {
            let views = self.link.as_ref().map(|link| link.workspace.views_of(terminal_id)).unwrap_or(0);
            if views > 0 {
                let status = exit_status.map(|code| format!("exit {code}")).or_else(|| signal.map(|code| format!("signal {code}"))).unwrap_or_else(|| "closed".into());
                let title = self.pane_title(Some(terminal_id));
                self.toast(ToastKind::Info, &format!("{title} ended"), Some(status));
            }
            self.agent_seen.remove(terminal_id);
            self.toasts.retain(|toast| toast.terminal.as_ref() != Some(terminal_id));
        }
        if let Event::Detached { message, .. } = event {
            self.toast(ToastKind::Error, "Detached", Some(message.clone()));
        }
        if let Some(message) = crate::connection::refused_input(event) {
            self.toast(ToastKind::Error, "Input not delivered", Some(message));
        }
        if let Event::InputDelivery { outcome: DeliveryOutcome::Refused, message, .. } = event {
            if crate::connection::refused_input(event).is_none() && !message.is_empty() {
                self.toast(ToastKind::Error, "Input not delivered", Some(message.clone()));
            }
        }
    }

    fn note_agents(&mut self) {
        let Some(link) = self.link.as_ref() else { return };
        let focused = link.workspace.focused().and_then(|placement| placement.terminal_id.clone());
        let current = link.connection.agents.clone();
        if !self.prefs.notifications {
            self.agent_seen = current.into_iter().map(|(id, info)| (id, info.state)).collect();
            return;
        }
        for (id, info) in &current {
            let previous = self.agent_seen.get(id).cloned();
            if previous.as_deref() == Some(info.state.as_str()) || previous.is_none() {
                continue;
            }
            let needs = info.state == "done" || info.state == "blocked" || info.attention == "high";
            if !needs || focused.as_ref() == Some(id) {
                continue;
            }
            let verb = match info.state.as_str() {
                "blocked" => "needs you",
                "done" => "finished",
                other => other,
            };
            let title = format!("{} {verb}", info.name);
            let body = format!("{} · {}", self.pane_title(Some(id)), short_path(self.pane_cwd(Some(id))));
            self.toast_agent(title, body, id.clone());
        }
        self.agent_seen = current.into_iter().map(|(id, info)| (id, info.state)).collect();
    }

    fn take_notices(&mut self) {
        let Some(link) = self.link.as_mut() else { return };
        for notice in link.workspace.take_notices() {
            let kind = if notice.kind == "error" { ToastKind::Error } else { ToastKind::Info };
            self.toast(kind, &notice.title, notice.body.clone());
        }
    }

    pub fn execute(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.last_run.as_ref().is_some_and(|(prev, at)| prev == id && now.duration_since(*at) < Duration::from_millis(8)) {
            return;
        }
        self.last_run = Some((id.to_owned(), now));
        if id == "escape" {
            self.escape(window, cx);
            return;
        }
        let typing = settings::text_field_has_keys(
            !matches!(self.modal, Modal::None),
            self.find.as_ref().map(|find| find.placement_id.as_str()),
            self.focused_placement().map(|placement| placement.id.as_str()),
        );
        if typing && settings::yields_to_text_field(id) {
            return;
        }
        let open = self.modal.clone();
        if !matches!(open, Modal::None) {
            let toggle = matches!(
                (&open, id),
                (Modal::Commands, "palette") | (Modal::Goto, "goto") | (Modal::Settings, "settings")
            );
            self.close_modal(window, cx);
            if toggle {
                return;
            }
        }
        if let Some(classified) = settings::classify_command(&canonical_id(id)) {
            self.run_classified(classified, window, cx);
            return;
        }
        self.run_catalog(id, window, cx);
    }

    fn run_classified(&mut self, command: settings::BoundAction, window: &mut Window, cx: &mut Context<Self>) {
        use settings::BoundAction::*;
        match command {
            Ignore => {}
            Send(text) => self.send_text(&text, cx),
            WriteFile { region, mode } => self.write_file(&region, &mode, window, cx),
            FontSize(size) => self.update_prefs(|prefs| prefs.font_size = size, cx),
            ScrollLines(rows) => self.scroll_lines(rows as i64),
            PromptJump(prompts) => self.jump_prompt(prompts as i64),
        }
    }

    fn run_catalog(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "palette" => self.modal = Modal::Commands,
            "goto" => self.modal = Modal::Goto,
            "settings" => self.modal = Modal::Settings,
            "new" => self.workspace_call(|link| link.workspace.new_terminal(&link.connection, None)),
            "new-home" => {
                let home = std::env::var("HOME").ok();
                self.workspace_call(|link| link.workspace.new_terminal(&link.connection, home.as_deref()));
            }
            "new-session" => self.modal = Modal::Session {
                create: true,
                current: String::new(),
            },
            "rename-session" => self.open_rename_session(),
            "folder" => self.open_folder(window, cx),
            "insert-path" => self.open_path_picker(),
            "split-right" => self.workspace_call(|link| link.workspace.split(&link.connection, Axis::Row, false)),
            "split-down" => self.workspace_call(|link| link.workspace.split(&link.connection, Axis::Column, false)),
            "split-left" => self.workspace_call(|link| link.workspace.split(&link.connection, Axis::Row, true)),
            "split-up" => self.workspace_call(|link| link.workspace.split(&link.connection, Axis::Column, true)),
            "view" => self.workspace_call(|link| link.workspace.duplicate_view(&link.connection)),
            "close" => self.workspace_call(|link| link.workspace.close_pane(&link.connection, None)),
            "close-tab" => {
                if let Some(id) = self.active_tab_id() {
                    self.workspace_mut(|link| link.workspace.close_tab(&link.connection, &id));
                }
            }
            "zoom" => self.workspace_mut(|link| link.workspace.toggle_zoom()),
            "equalize" => {
                self.workspace_mut(|link| link.workspace.equalize_tab());
                self.persist();
            }
            "pane-left" => self.workspace_mut(|link| link.workspace.focus_direction(Direction::Left)),
            "pane-right" => self.workspace_mut(|link| link.workspace.focus_direction(Direction::Right)),
            "pane-up" => self.workspace_mut(|link| link.workspace.focus_direction(Direction::Up)),
            "pane-down" => self.workspace_mut(|link| link.workspace.focus_direction(Direction::Down)),
            "pane-next" => self.workspace_mut(|link| link.workspace.cycle_pane(1)),
            "pane-prev" => self.workspace_mut(|link| link.workspace.cycle_pane(-1)),
            "tab-next" => self.workspace_mut(|link| link.workspace.step_tab(1)),
            "tab-prev" => self.workspace_mut(|link| link.workspace.step_tab(-1)),
            "tab-move-left" => self.move_active_tab(-1),
            "tab-move-right" => self.move_active_tab(1),
            "rename" => self.rename_active(),
            "window" => self.move_to_window(cx),
            "find" => self.open_find(),
            "find-next" => self.step_find(1),
            "find-prev" => self.step_find(-1),
            "find-selection" => self.find_selection(),
            "find-close" => self.close_find(),
            "follow" | "scroll-bottom" => self.follow(),
            "paste-cwd" => self.paste_cwd(),
            "terminate" => self.ask_terminate(),
            "attention" => self.next_attention(),
            "sidebar" => self.update_prefs(|prefs| prefs.sidebar_visible = !prefs.sidebar_visible, cx),
            "font-up" => self.update_prefs(|prefs| prefs.font_size += 1.0, cx),
            "font-down" => self.update_prefs(|prefs| prefs.font_size -= 1.0, cx),
            "font-reset" => {
                let size = DisplayPrefs::default().font_size;
                self.update_prefs(|prefs| prefs.font_size = size, cx);
            }
            "option-alt" => self.update_prefs(|prefs| prefs.option_as_alt = !prefs.option_as_alt, cx),
            "notify" => self.update_prefs(|prefs| prefs.notifications = !prefs.notifications, cx),
            "scroll-page-up" => self.scroll_page(-1),
            "scroll-page-down" => self.scroll_page(1),
            "scroll-top" => self.scroll_top(),
            "clear" => self.send_text("\u{000c}", cx),
            "select-all" => self.select_all(),
            "copy" => self.host_action(HostAction::Copy, cx),
            "paste" => self.host_action(HostAction::Paste, cx),
            "paste-selection" => self.paste_selection(),
            "prompt-prev" => self.jump_prompt(-1),
            "prompt-next" => self.jump_prompt(1),
            "quick-terminal" => self.toggle_quick(cx),
            "new-window" => self.modal = Modal::WindowSession,
            "fullscreen" => window.toggle_fullscreen(),
            "maximize" => window.zoom_window(),
            "reload-config" => self.reload_ghostty(cx),
            "import-ghostty" => self.import_ghostty(cx),
            "reconnect" => self.reconnect(cx),
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

    fn after_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.take_notices();
        self.retain_panes();
        self.focus_terminal(window, cx);
        self.sync_title(window);
        cx.notify();
    }

    pub fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let chord = shell_chord(
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
            event.keystroke.modifiers.alt,
            event.keystroke.modifiers.shift,
            event.keystroke.key.as_ref(),
            event.is_held,
        );
        if chord.is_empty() {
            return;
        }
        let settings_chord = settings::key_chord(&settings::KeyChord {
            key: event.keystroke.key.to_string(),
            held: event.is_held,
            cmd: event.keystroke.modifiers.platform,
            alt: event.keystroke.modifiers.alt,
            ctrl: event.keystroke.modifiers.control,
            shift: event.keystroke.modifiers.shift,
        });
        let chord = if settings_chord.is_empty() { chord } else { settings_chord };
        if let Some(id) = self.binding(&chord) {
            window.dispatch_action(Box::new(Run::new(id)), cx);
            cx.stop_propagation();
        }
    }

    fn binding(&self, chord: &str) -> Option<String> {
        let ghostty = self.ghostty();
        settings::effective_keymap(&ghostty, self.prefs.ghostty_keys)
            .into_iter()
            .find(|(bound, _)| bound == chord)
            .map(|(_, id)| id)
    }

    pub fn sync_bindings(&self, cx: &mut Context<Self>) {
        let desktop = self.desktop.clone();
        let ghostty = self.ghostty();
        let keys = self.prefs.ghostty_keys;
        desktop.update(cx, |desktop, cx| desktop.install_bindings(&ghostty, keys, cx));
    }

    fn ghostty(&self) -> settings::GhosttyConfig {
        settings::parse_ghostty(settings::read_ghostty_config().as_deref().unwrap_or(""))
    }

    pub fn app_chords(&self) -> Vec<String> {
        let ghostty = self.ghostty();
        let keymap = settings::effective_keymap(&ghostty, self.prefs.ghostty_keys);
        settings::app_chords(&keymap)
    }

    pub fn shown_chord(&self, id: &str) -> Option<String> {
        let ghostty = self.ghostty();
        let keymap = settings::effective_keymap(&ghostty, self.prefs.ghostty_keys);
        settings::shown_chords(&keymap, &settings::default_chords())
            .into_iter()
            .find(|(command, _)| command == id)
            .map(|(_, chord)| settings::display_chord(&chord))
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.modal, Modal::None) {
            self.close_modal(window, cx);
        } else if self.find.is_some() {
            self.close_find();
            cx.notify();
        }
    }

    pub fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Modal::Path) {
            self.picker = None;
        }
        self.modal = Modal::None;
        self.focus_terminal(window, cx);
        cx.notify();
    }

    pub(crate) fn workspace_call(&mut self, call: impl FnOnce(&mut Link)) {
        if let Some(link) = self.link.as_mut() {
            call(link);
        }
    }

    fn workspace_mut(&mut self, call: impl FnOnce(&mut Link)) {
        self.workspace_call(call);
    }

    pub(crate) fn update_prefs(&mut self, change: impl FnOnce(&mut DisplayPrefs), cx: &mut Context<Self>) {
        change(&mut self.prefs);
        self.prefs = settings::sanitize_display(&self.prefs);
        self.persist();
        self.sync_bindings(cx);
        cx.notify();
    }

    fn persist(&mut self) {
        if !self.persist_writes {
            return;
        }
        let attached = self.link.as_ref().is_some_and(|link| {
            link.connection.status == Status::Attached
                && link.workspace.server_id() == link.connection.server_id().as_deref()
        });
        if attached {
            if let Some(link) = self.link.as_ref() {
                if let Some(server) = link.connection.server_id() {
                    self.last_saved = Some(persist::save_layout(
                        &server,
                        &link.workspace.tabs,
                        &self.prefs,
                        Some(link.workspace.active_id.as_str()),
                    ));
                }
            }
        } else if let Some(saved) = self.last_saved.as_mut() {
            saved.display = self.prefs.clone();
        } else if self.link.as_ref().and_then(|link| link.connection.server_id()).is_none() {
            self.last_saved = Some(persist::save_layout("", &[], &self.prefs, None));
        } else {
            return;
        }
        if let Some(saved) = &self.last_saved {
            if let Err(error) = self.store.write(&persist::layout_value(saved)) {
                self.toast(ToastKind::Error, "Could not save desktop layout", Some(error.to_string()));
            }
        }
    }

    pub fn prepare_close(&mut self, cx: &mut Context<Self>) {
        self.persist();
        if let Some(link) = self.link.as_mut() {
            link.workspace.release_all(&link.connection);
            link.connection.close();
        }
        self.panes.clear();
        cx.notify();
    }

    fn reconnect(&mut self, cx: &mut Context<Self>) {
        if self.link.as_ref().is_none_or(|link| link.connection.status != Status::Attached) {
            if let Some(error) = host::ensure_server(&self.phux, &self.socket, &self.session) {
                self.toast(ToastKind::Error, "Could not start the phux server", Some(error));
            }
        }
        self.persist();
        let snapshot = self.last_saved.clone();
        let closed = self.link.as_ref().is_some_and(|link| {
            matches!(link.connection.status, Status::Closed | Status::Failed)
        });
        if closed || self.link.is_none() {
            if let Some(link) = self.link.as_mut() {
                link.workspace.release_all(&link.connection);
                link.connection.close();
            }
            self.panes.clear();
            self.agent_seen.clear();
            self.toasts.retain(|toast| toast.kind != ToastKind::Agent);
            self.find = None;
            self.attach(false, None, cx);
            if let Some(link) = self.link.as_mut() {
                link.workspace.restore(snapshot);
            }
        } else if let Some(link) = self.link.as_mut() {
            link.workspace.release_all(&link.connection);
            self.panes.clear();
            self.agent_seen.clear();
            self.toasts.retain(|toast| toast.kind != ToastKind::Agent);
            self.find = None;
            link.workspace.restore(snapshot);
            if let Err(error) = link.connection.reconnect() {
                self.toast(ToastKind::Error, "Could not reconnect", Some(error.to_string()));
            }
        }
        self.toast(ToastKind::Info, "Reconnecting", Some(self.socket.display().to_string()));
        cx.notify();
    }

    fn reload_ghostty(&mut self, cx: &mut Context<Self>) {
        let text = settings::read_ghostty_config();
        self.ghostty_found = text.is_some();
        self.sync_bindings(cx);
        if text.is_none() {
            self.toast(ToastKind::Info, "No Ghostty config found", None);
        } else {
            self.toast(ToastKind::Success, "Ghostty config reloaded", None);
        }
        cx.notify();
    }

    fn import_ghostty(&mut self, cx: &mut Context<Self>) {
        let ghostty = self.ghostty();
        let next = settings::ghostty_prefs(&self.prefs, &ghostty);
        self.prefs = settings::sanitize_display(&next);
        self.persist();
        self.sync_bindings(cx);
        cx.notify();
    }

    fn toggle_quick(&self, cx: &mut Context<Self>) {
        self.desktop.update(cx, |desktop, cx| desktop.toggle_quick(cx));
    }

    fn move_to_window(&mut self, cx: &mut Context<Self>) {
        let Some(placement) = self.focused_placement().cloned() else { return };
        let Some(terminal) = placement.terminal_id.clone() else { return };
        let session = self.session.clone();
        self.workspace_call(|link| link.workspace.close_pane(&link.connection, Some(placement.id.as_str())));
        self.desktop.update(cx, |desktop, cx| {
            desktop.open_moved(session, terminal, cx);
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
            let paths = prompt.await.ok().and_then(Result::ok).flatten().unwrap_or_default();
            if let Some(path) = paths.into_iter().next() {
                let _ = desk.update(cx, |desk, cx| {
                    let cwd = path.display().to_string();
                    desk.workspace_call(|link| {
                        link.workspace.new_terminal(&link.connection, Some(cwd.as_str()))
                    });
                    desk.take_notices();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn open_rename_session(&mut self) {
        let name = self.focused_pane().map(|pane| pane.session_name.clone()).unwrap_or_default();
        if name.trim().is_empty() {
            self.toast(ToastKind::Error, "No session", Some("Focus a pane in the session you want to rename.".into()));
            return;
        }
        self.modal = Modal::Session { create: false, current: name };
    }

    fn rename_active(&mut self) {
        if let Some(tab) = self.active_tab() {
            self.modal = Modal::Rename { tab_id: tab.id.clone() };
        }
    }

    fn ask_terminate(&mut self) {
        let Some(placement) = self.focused_placement() else { return };
        let Some(terminal) = placement.terminal_id.clone() else { return };
        let title = self.pane_title(Some(&terminal));
        self.modal = Modal::Terminate { terminal, title };
    }

    pub fn confirm_terminate(&mut self, cx: &mut Context<Self>) {
        let Modal::Terminate { terminal, .. } = self.modal.clone() else { return };
        self.modal = Modal::None;
        self.workspace_call(|link| {
            let _ = link.workspace.terminate(&link.connection, &terminal);
        });
        self.take_notices();
        cx.notify();
    }

    pub fn apply_rename(&mut self, title: String, cx: &mut Context<Self>) {
        if let Modal::Rename { tab_id } = self.modal.clone() {
            self.workspace_mut(|link| link.workspace.rename_tab(&tab_id, &title));
        }
        self.modal = Modal::None;
        self.persist();
        cx.notify();
    }

    pub fn apply_session(&mut self, name: String, directory: Option<String>, cx: &mut Context<Self>) {
        let Modal::Session { create, current } = self.modal.clone() else { return };
        let failure = if create {
            host::create_session(&self.phux, &self.socket, &name, directory.as_deref().unwrap_or(""))
        } else {
            host::rename_session(&self.phux, &self.socket, &current, &name)
        };
        if let Some(failure) = failure {
            self.toast(
                ToastKind::Error,
                if create { "Could not create the session" } else { "Could not rename the session" },
                Some(failure),
            );
            return;
        }
        self.modal = Modal::None;
        if let Some(link) = self.link.as_ref() {
            let _ = link.connection.refresh_topology();
        }
        self.toast(
            ToastKind::Success,
            if create { "Session created" } else { "Session renamed" },
            Some(name.trim().to_owned()),
        );
        cx.notify();
    }

    pub fn confirm_close_session(&mut self, cx: &mut Context<Self>) {
        let Modal::CloseSession { name } = self.modal.clone() else { return };
        self.modal = Modal::None;
        if let Some(failure) = host::close_session(&self.phux, &self.socket, &name) {
            self.toast(ToastKind::Error, "Could not close the session", Some(failure));
            return;
        }
        if let Some(link) = self.link.as_ref() {
            let _ = link.connection.refresh_topology();
        }
        self.toast(ToastKind::Success, "Session closed", Some(name));
        cx.notify();
    }

    pub fn open_session_window(&mut self, name: String, cx: &mut Context<Self>) {
        self.modal = Modal::None;
        let session = if name.trim().is_empty() { self.session.clone() } else { name };
        self.desktop.update(cx, |desktop, cx| desktop.open_session(session, cx));
        cx.notify();
    }

    fn move_active_tab(&mut self, step: i8) {
        let Some(link) = self.link.as_ref() else { return };
        let len = link.workspace.tabs.len();
        if len < 2 {
            return;
        }
        let from = link.workspace.tabs.iter().position(|tab| tab.id == link.workspace.active_id);
        let Some(from) = from else { return };
        let to = (from as i32 + step as i32).rem_euclid(len as i32) as usize;
        self.workspace_mut(|link| link.workspace.move_tab(from, to));
    }

    fn next_attention(&mut self) {
        let Some(link) = self.link.as_ref() else { return };
        let order = ["blocked", "done", "working"];
        let mut panes: Vec<_> = link.connection.panes().iter().filter(|pane| {
            link.connection.agents.get(&pane.terminal_id).is_some_and(|agent| order.contains(&agent.state.as_str()))
        }).cloned().collect();
        panes.sort_by(|left, right| {
            let rank = |pane: &phux_client_runtime::control::PaneDescriptor| {
                link.connection.agents.get(&pane.terminal_id).map(|agent| {
                    order.iter().position(|state| *state == agent.state).unwrap_or(9)
                }).unwrap_or(9)
            };
            rank(left).cmp(&rank(right)).then_with(|| {
                let changed = |pane: &phux_client_runtime::control::PaneDescriptor| {
                    link.connection.agents.get(&pane.terminal_id).map(|agent| agent.changed_at_ms).unwrap_or(0)
                };
                changed(right).cmp(&changed(left))
            })
        });
        if let Some(pane) = panes.first() {
            self.reveal_terminal(&pane.terminal_id);
        } else {
            self.toast(ToastKind::Info, "No agent needs attention", None);
        }
    }

    pub fn reveal_terminal(&mut self, terminal: &ResourceId) {
        let label = self.link.as_ref().and_then(|link| {
            link.workspace.tabs.iter().find_map(|tab| {
                crate::workspace::layout::placements(&tab.root).into_iter().find(|placement| placement.terminal_id.as_ref() == Some(terminal)).map(|placement| placement.terminal_label.clone())
            })
        });
        self.workspace_call(|link| {
            if let Some(pane) = link.connection.pane(terminal).cloned() {
                link.workspace.open(&link.connection, &pane);
            } else if let Some(label) = label {
                let _ = link.workspace.reveal(&label);
            }
        });
        self.modal = Modal::None;
    }

    pub fn select_tab(&mut self, id: &str) {
        self.workspace_mut(|link| link.workspace.select_tab(id));
    }

    pub fn close_tab(&mut self, id: &str) {
        self.workspace_mut(|link| link.workspace.close_tab(&link.connection, id));
    }

    pub fn focus_placement(&mut self, tab_id: &str, placement_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace_mut(|link| link.workspace.focus_placement(tab_id, placement_id));
        self.focus_terminal(window, cx);
        cx.notify();
    }

    fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.modal, Modal::None) {
            return;
        }
        if self.find.is_some() {
            return;
        }
        if let Some(id) = self.focused_placement().map(|placement| placement.id.clone()) {
            self.panes.focus(&id, window, cx);
        }
    }

    fn host_action(&mut self, action: HostAction, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_placement().filter(|placement| placement.view_id.is_some()).map(|placement| placement.id.clone()) {
            self.panes.request(&id, action, cx);
        }
    }

    fn send_text(&mut self, text: &str, _cx: &mut Context<Self>) {
        let Some(terminal) = self.focused_terminal() else { return };
        let events = typed_keys(text);
        let client = self.client();
        let Some(client) = client else { return };
        let sent = client.with_control(|control| {
            events.into_iter().all(|event| control.send_key(&terminal, event))
        });
        if !sent {
            self.toast(ToastKind::Error, "Could not send keys", Some("The terminal did not accept the input.".into()));
        }
        client.wake();
    }

    fn scroll_lines(&mut self, rows: i64) {
        self.with_view(|client, view| {
            let _ = client.scroll_view(view, Scroll::Delta(rows));
        });
    }

    fn scroll_page(&mut self, direction: i64) {
        self.with_view(|client, view| {
            let rows = client.acquire_view(view).map(|frame| frame.rows).unwrap_or(1);
            let _ = client.scroll_view(view, Scroll::Delta(direction * i64::from(rows.saturating_sub(1)).max(1)));
        });
    }

    fn scroll_top(&mut self) {
        self.with_view(|client, view| {
            let _ = client.scroll_view(view, Scroll::Top);
        });
    }

    fn follow(&mut self) {
        self.with_view(|client, view| {
            let _ = client.follow_live_view(view);
        });
    }

    fn jump_prompt(&mut self, prompts: i64) {
        self.with_view(|client, view| {
            let _ = client.scroll_view(view, Scroll::Prompt(prompts));
        });
    }

    fn select_all(&mut self) {
        self.with_engine(|engine, view| {
            let _ = engine.select_all_view(view);
        });
    }

    fn paste_selection(&mut self) {
        let Some(text) = self.selection_text() else { return };
        if text.is_empty() {
            return;
        }
        self.paste_text(&text);
    }

    fn paste_cwd(&mut self) {
        let cwd = self.focused_pane().and_then(|pane| pane.cwd.clone());
        let Some(cwd) = cwd else { return };
        self.paste_text(&host::quote_dropped_paths(&[PathBuf::from(cwd)]));
    }

    pub fn drop_paths(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        self.paste_text(&host::quote_dropped_paths(paths));
    }

    fn paste_text(&mut self, text: &str) {
        let Some(terminal) = self.focused_terminal() else { return };
        let Some(client) = self.client() else { return };
        let queued = client.with_control(|control| {
            if !control.input_ready(&terminal) {
                return false;
            }
            let _ = control.apply_paste(&terminal, text);
            true
        });
        if queued {
            client.wake();
        } else {
            self.toast(ToastKind::Error, "Could not paste", Some("The terminal is not ready.".into()));
        }
    }

    fn write_file(&mut self, region: &str, mode: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.focused_view() else { return };
        let Some(client) = self.client() else { return };
        let text = match region {
            "selection" => self.selection_text().unwrap_or_default(),
            "scrollback" => region_text(&client, view, TextRegion::History),
            _ => region_text(&client, view, TextRegion::Screen),
        };
        if text.is_empty() {
            self.toast(ToastKind::Info, if region == "selection" { "Nothing selected" } else { "Nothing to write" }, None);
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
                self.toast(ToastKind::Error, "Could not write the file", Some(error));
                return;
            }
        };
        let shown = path.display().to_string();
        match mode {
            "paste" => self.paste_text(&host::quote_dropped_paths(&[path])),
            "copy" => self.host_action(HostAction::CopyText(shown), cx),
            _ => self.host_action(HostAction::Open(shown), cx),
        }
    }

    fn open_find(&mut self) {
        let Some(placement) = self.focused_placement() else { return };
        if self.find.as_ref().is_some_and(|find| find.placement_id == placement.id) {
            let query = self.find.as_ref().map(|find| find.query.clone()).unwrap_or_default();
            let case = self.find.as_ref().map(|find| find.case_sensitive).unwrap_or(false);
            self.run_search(query, case);
            return;
        }
        self.find = Some(FindState {
            placement_id: placement.id.clone(),
            query: String::new(),
            case_sensitive: false,
            matches: Vec::new(),
            index: 0,
        });
    }

    fn find_selection(&mut self) {
        let Some(placement) = self.focused_placement().cloned() else { return };
        let selected = self.selection_text().unwrap_or_default();
        let query = selected.split(['\n', '\r']).find(|line| !line.trim().is_empty()).unwrap_or("").chars().take(512).collect::<String>();
        if query.is_empty() {
            self.open_find();
            return;
        }
        let case = self.find.as_ref().map(|find| find.case_sensitive).unwrap_or(false);
        self.find = Some(FindState { placement_id: placement.id, query: query.clone(), case_sensitive: case, matches: Vec::new(), index: 0 });
        self.run_search(query, case);
    }

    pub fn run_search(&mut self, query: String, case_sensitive: bool) {
        let Some(state) = self.find.clone() else { return };
        let Some(placement) = self.placement(&state.placement_id).cloned() else { return };
        let Some(view) = placement.view_id else { return };
        let matches = if query.is_empty() {
            Vec::new()
        } else {
            self.client().and_then(|client| client.engine()).and_then(|engine| engine.search_view(view, query.clone(), case_sensitive).ok()).unwrap_or_default()
                .into_iter().map(|found| (found.start, found.end)).collect()
        };
        self.show_match(&placement, &matches, 0);
        self.find = Some(FindState { placement_id: state.placement_id, query, case_sensitive, matches, index: 0 });
    }

    fn step_find(&mut self, delta: i32) {
        if self.find.is_none() {
            self.open_find();
            return;
        }
        let Some(state) = self.find.clone() else { return };
        if state.matches.is_empty() {
            self.run_search(state.query, state.case_sensitive);
            return;
        }
        let len = state.matches.len() as i32;
        let index = (state.index as i32 + delta).rem_euclid(len) as usize;
        if let Some(placement) = self.placement(&state.placement_id).cloned() {
            self.show_match(&placement, &state.matches, index);
        }
        self.find = Some(FindState { index, ..state });
    }

    fn show_match(&self, placement: &Placement, matches: &[(u64, u64)], index: usize) {
        let Some(view) = placement.view_id else { return };
        let Some(client) = self.client() else { return };
        let Some(engine) = client.engine() else { return };
        if let Some((start, end)) = matches.get(index) {
            let _ = engine.set_view_selection(view, *start, *end, false);
            let _ = client.pin_viewport_view(view, *start);
        } else {
            let _ = engine.clear_view_selection(view);
        }
    }

    fn close_find(&mut self) {
        if let Some(state) = self.find.take() {
            if let Some(placement) = self.placement(&state.placement_id).cloned() {
                if let Some(view) = placement.view_id {
                    if let Some(engine) = self.client().and_then(|client| client.engine()) {
                        let _ = engine.clear_view_selection(view);
                    }
                }
            }
        }
    }

    pub fn find_status(&self) -> String {
        let Some(state) = &self.find else { return String::new() };
        if state.query.is_empty() {
            return String::new();
        }
        if state.matches.is_empty() {
            return "no matches".into();
        }
        format!("{} of {}{}", state.index + 1, state.matches.len(), if state.matches.len() >= 4096 { "+" } else { "" })
    }

    fn open_path_picker(&mut self) {
        let Some(target) = self.path_target() else {
            self.toast(ToastKind::Info, "Insert Path is unavailable", Some("Not attached to a terminal.".into()));
            return;
        };
        let features = self.link.as_ref().map(|link| link.connection.features()).unwrap_or_default();
        let attached = self.link.as_ref().is_some_and(|link| link.connection.status == Status::Attached);
        if !paths::can_offer_path_picker(Some(features.as_slice()), attached, Some(&target)) {
            self.toast(ToastKind::Info, "Insert Path is unavailable", Some("This server does not offer host path search. Update phux on the host.".into()));
            return;
        }
        let root = self.pane_cwd(crate::connection::parse_terminal(&target.terminal_id).as_ref()).unwrap_or_else(|| "~".into());
        self.picker = Some(paths::begin_picker(target, root.clone()));
        self.modal = Modal::Path;
        self.query_paths(root, String::new());
    }

    pub fn query_paths(&mut self, root: String, query: String) {
        let Some(active) = self.picker.clone() else { return };
        let Some(current) = self.path_target() else {
            self.close_path_picker();
            return;
        };
        if !paths::same_target(&active.target, Some(&current)) {
            self.close_path_picker();
            return;
        }
        let host = paths::host_for_terminal(&current.terminal_id).map(SatelliteHost::new);
        let id = self.link.as_ref().map(|link| link.connection.path_query(root.clone(), query.clone(), !query.is_empty(), host)).unwrap_or(0);
        self.picker = Some(if id == 0 {
            let mut failed = active;
            failed.root = root;
            failed.query = query;
            failed.pending = None;
            failed.rows.clear();
            failed.message = "Path search is unavailable on this server.".into();
            failed
        } else {
            paths::start_query(active, id, &query)
        });
    }

    pub fn insert_path(&mut self, path: String) {
        let Some(active) = self.picker.clone() else { return };
        let current = self.path_target();
        let ready = current.as_ref().and_then(|target| crate::connection::parse_terminal(&target.terminal_id)).is_some_and(|id| {
            self.link.as_ref().is_some_and(|link| link.connection.ready(&id))
        });
        let Some(text) = paths::prepared_insertion(Some(&active), &path, current.as_ref(), ready) else {
            let mut rejected = active;
            rejected.message = "The pane changed or is not ready; nothing was inserted.".into();
            self.picker = Some(rejected);
            return;
        };
        let before = self.toasts.len();
        self.paste_text(&text);
        if self.toasts.len() == before {
            self.close_path_picker();
        }
    }

    pub fn open_path_terminal(&mut self, directory: String) {
        self.workspace_call(|link| link.workspace.new_terminal(&link.connection, Some(directory.as_str())));
        self.close_path_picker();
        self.take_notices();
    }

    fn close_path_picker(&mut self) {
        self.picker = None;
        if matches!(self.modal, Modal::Path) {
            self.modal = Modal::None;
        }
    }

    fn pin_picker(&mut self) {
        let Some(active) = self.picker.clone() else { return };
        let attached = self.link.as_ref().is_some_and(|link| link.connection.status == Status::Attached);
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

    pub fn terminal_settings(&self, placement: &Placement, focused: bool) -> Option<TerminalSettings> {
        let terminal = placement.terminal_id.clone()?;
        let colors = self.palette();
        let opacity = crate::settings::theme::with_alpha(&colors.theme.background, self.prefs.terminal_opacity);
        let mut settings = TerminalSettings::default();
        settings.font = gpui::font(self.prefs.font_family.clone());
        settings.font_size = self.prefs.font_size as f32;
        settings.line_height = self.prefs.line_height as f32;
        settings.cell_width_scale = self.prefs.cell_width as f32;
        settings.cell_height_scale = self.prefs.cell_height as f32;
        settings.foreground = Some(super::color::from_hex(&colors.theme.foreground));
        settings.background = Some(super::color::from_hex(&opacity));
        settings.cursor = Some(super::color::from_hex(&colors.theme.cursor));
        settings.selection_background = super::color::from_hex(&colors.theme.selection);
        settings.selection_foreground = super::color::from_hex(colors.theme.selection_foreground.as_deref().unwrap_or(&colors.theme.foreground));
        settings.palette = colors.theme.palette.as_ref().and_then(|entries| {
            let mut parsed = [gpui::Hsla::default(); 16];
            if entries.len() != 16 {
                return None;
            }
            for (slot, color) in parsed.iter_mut().zip(entries) {
                *slot = super::color::from_hex(color);
            }
            Some(parsed)
        });
        settings.focused = focused;
        settings.cursor_visible = true;
        settings.blink_visible = true;
        settings.option_as_alt = self.prefs.option_as_alt;
        settings.size_owner = self.link.as_ref().is_some_and(|link| link.workspace.size_owner(placement));
        settings.app_chords = self.app_chords();
        let _ = terminal;
        Some(settings)
    }

    pub fn palette(&self) -> crate::settings::theme::Palette {
        let extra = settings::ghostty_theme(&self.ghostty());
        let extras = extra.as_ref().map(std::slice::from_ref).unwrap_or(&[]);
        let theme = crate::settings::theme::theme_by_id(&self.prefs.theme_id, extras);
        crate::settings::theme::palette(&theme)
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        self.link.as_ref().map(|link| link.connection.client.clone())
    }

    fn with_view(&self, call: impl FnOnce(&Client, phux_client_runtime::ViewId)) {
        let Some(view) = self.focused_view() else { return };
        let Some(client) = self.client() else { return };
        call(&client, view);
    }

    fn with_engine(&self, call: impl FnOnce(&phux_client_runtime::engine::EngineHandle, phux_client_runtime::ViewId)) {
        self.with_view(|client, view| {
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
            crate::workspace::layout::placements(&tab.root).into_iter().find(|placement| placement.id == id)
        })
    }

    pub(crate) fn active_tab(&self) -> Option<&crate::workspace::layout::DeskTab> {
        self.link.as_ref().and_then(|link| link.workspace.active_tab())
    }

    pub(crate) fn active_tab_id(&self) -> Option<String> {
        self.active_tab().map(|tab| tab.id.clone())
    }

    pub fn pane_title(&self, terminal: Option<&ResourceId>) -> String {
        let Some(terminal) = terminal else { return "terminal".into() };
        let title = self.link.as_ref().and_then(|link| link.connection.pane(terminal)).and_then(|pane| pane.title.as_deref().map(str::trim)).filter(|title| !title.is_empty());
        title.map(str::to_owned).unwrap_or_else(|| "terminal".into())
    }

    fn pane_cwd(&self, terminal: Option<&ResourceId>) -> Option<String> {
        let terminal = terminal?;
        self.link.as_ref().and_then(|link| link.connection.pane(terminal)).and_then(|pane| pane.cwd.clone())
    }

    fn focused_pane(&self) -> Option<&phux_client_runtime::control::PaneDescriptor> {
        let terminal = self.focused_terminal()?;
        self.link.as_ref().and_then(|link| link.connection.pane(&terminal))
    }

    pub fn agent(&self, terminal: &ResourceId) -> Option<&AgentInfo> {
        self.link.as_ref().and_then(|link| link.connection.agents.get(terminal))
    }

    pub fn toast(&mut self, kind: ToastKind, title: &str, body: Option<String>) {
        self.toast_id += 1;
        self.toasts.push(Toast { id: self.toast_id, kind, title: title.to_owned(), body, at: Instant::now(), terminal: None });
        self.toasts = self.toasts.split_off(self.toasts.len().saturating_sub(TOAST_LIMIT));
    }

    fn toast_agent(&mut self, title: String, body: String, terminal: ResourceId) {
        self.toast_id += 1;
        self.toasts.push(Toast { id: self.toast_id, kind: ToastKind::Agent, title, body: Some(body), at: Instant::now(), terminal: Some(terminal) });
        self.toasts = self.toasts.split_off(self.toasts.len().saturating_sub(TOAST_LIMIT));
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|toast| toast.id != id);
    }

    fn ensure_clock(&mut self, cx: &mut Context<Self>) {
        let needed = !self.toasts.is_empty() || self.link.as_ref().is_some_and(|link| !link.connection.agents.is_empty());
        if !needed || self._clock.is_some() {
            return;
        }
        self._clock = Some(cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                let keep = this.update(cx, |desk, cx| {
                    desk.now = Instant::now();
                    desk.toasts.retain(|toast| {
                        let limit = if toast.kind == ToastKind::Agent || toast.kind == ToastKind::Error { 12 } else { 5 };
                        desk.now.duration_since(toast.at) < Duration::from_secs(limit)
                    });
                    let keep = !desk.toasts.is_empty() || desk.link.as_ref().is_some_and(|link| !link.connection.agents.is_empty());
                    cx.notify();
                    keep
                }).unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn retain_panes(&mut self) {
        let live = self.link.as_ref().map(|link| {
            link.workspace.tabs.iter().flat_map(|tab| crate::workspace::layout::placements(&tab.root)).map(|placement| placement.id.clone()).collect::<Vec<_>>()
        }).unwrap_or_default();
        self.panes.retain(&live);
    }

    pub fn panes(&self) -> &Panes {
        &self.panes
    }

    pub fn panes_mut(&mut self) -> &mut Panes {
        &mut self.panes
    }

    fn sync_title(&self, window: &mut Window) {
        let title = self.active_tab().map(|tab| tab_title(self, tab)).unwrap_or_else(|| "phux".into());
        window.set_window_title(&format!("{title} — phux"));
    }

    pub fn set_content_bounds(&mut self, origin: gpui::Point<gpui::Pixels>, size: gpui::Size<gpui::Pixels>) {
        self.content_origin = origin;
        self.content_size = size;
    }

    pub fn set_viewport(&mut self, size: gpui::Size<gpui::Pixels>) {
        self.viewport = size;
    }

    pub fn frame_report(&self, cx: &gpui::App) -> crate::diag::FrameReport {
        let mut placements = Vec::new();
        let mut painted = Vec::new();
        if let Some(link) = &self.link {
            if let Some(tab) = link.workspace.tabs.iter().find(|tab| tab.id == link.workspace.active_id) {
                for placement in crate::workspace::layout::placements(&tab.root) {
                    placements.push((placement.id.clone(), placement.terminal_label.clone()));
                    let Some(entity) = self.panes.get(&placement.id) else { continue };
                    let observation = entity.read(cx).painted();
                    let bounds = observation.geometry.bounds;
                    painted.push(crate::diag::PaintedPane {
                        placement_id: placement.id.clone(),
                        terminal_id: placement.terminal_label.clone(),
                        generation: observation.frame.as_ref().map(|frame| frame.generation).unwrap_or(0),
                        bounds,
                        unclipped: pane_unclipped(self.viewport, bounds),
                    });
                }
            }
        }
        crate::diag::FrameReport { session: self.session.clone(), placements, painted }
    }

    pub fn set_tab_centers(&mut self, centers: Vec<(String, f32)>) {
        self.tab_centers = centers;
    }

    pub fn begin_sidebar_drag(&mut self) {
        self.drag = Some(Drag::Sidebar);
    }

    pub fn begin_split_drag(&mut self, id: String, axis: Axis) {
        self.drag = Some(Drag::Split { id, axis });
    }

    pub fn begin_tab_drag(&mut self, id: String, x: f32) {
        self.drag = Some(Drag::Tab { id, start_x: x, moved: false });
    }

    pub fn drag_move(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        match self.drag.as_mut() {
            Some(Drag::Sidebar) => {
                let width = f64::from(x).round().clamp(crate::settings::SIDEBAR_MIN, crate::settings::SIDEBAR_MAX);
                self.prefs.sidebar_width = width;
                cx.notify();
            }
            Some(Drag::Split { id, axis }) => {
                let id = id.clone();
                let axis = *axis;
                let ratio = match axis {
                    Axis::Row => (x - self.content_origin.x.as_f32()) / self.content_size.width.as_f32().max(1.),
                    Axis::Column => (y - self.content_origin.y.as_f32()) / self.content_size.height.as_f32().max(1.),
                };
                self.workspace_mut(|link| link.workspace.resize_split(&id, ratio));
                cx.notify();
            }
            Some(Drag::Tab { id, start_x, moved }) => {
                if !*moved && (x - *start_x).abs() < 6. {
                    return;
                }
                *moved = true;
                let id = id.clone();
                let target = self.tab_centers.iter().position(|(tab, center)| tab != &id && x > *center).unwrap_or(0);
                if let Some(link) = self.link.as_ref() {
                    if let Some(from) = link.workspace.tabs.iter().position(|tab| tab.id == id) {
                        if from != target {
                            self.workspace_mut(|link| link.workspace.move_tab(from, target));
                        }
                    }
                }
                cx.notify();
            }
            None => {}
        }
    }

    pub fn end_drag(&mut self) {
        if self.drag.take().is_some() {
            self.persist();
        }
    }

    pub fn new_terminal_in(&mut self, session_id: u32) {
        self.workspace_call(|link| link.workspace.new_terminal_in(&link.connection, session_id, None));
        self.take_notices();
    }

    pub fn split_focused(&mut self, axis: Axis) {
        self.workspace_call(|link| link.workspace.split(&link.connection, axis, false));
        self.take_notices();
    }

    pub(crate) fn active_tab_id_owned(&self) -> Option<String> {
        self.active_tab_id()
    }

    pub(crate) fn workspace_close(&mut self, id: &str) {
        self.workspace_call(|link| link.workspace.close_pane(&link.connection, Some(id)));
    }

    pub(crate) fn subscribed_terminal(&self, id: &str) -> bool {
        self.subscribed.iter().any(|known| known == id)
    }

    pub(crate) fn remember_terminal_sub(&mut self, id: String, sub: Subscription) {
        self.subscribed.push(id);
        self._subs.push(sub);
    }
}

fn canonical_id(id: &str) -> String {
    match id {
        "write-screen-copy" => "write-file:screen:copy".into(),
        "write-screen-paste" => "write-file:screen:paste".into(),
        "write-screen-open" => "write-file:screen:open".into(),
        "write-scrollback-copy" => "write-file:scrollback:copy".into(),
        "write-scrollback-paste" => "write-file:scrollback:paste".into(),
        "write-scrollback-open" => "write-file:scrollback:open".into(),
        "write-selection-copy" => "write-file:selection:copy".into(),
        "write-selection-paste" => "write-file:selection:paste".into(),
        "write-selection-open" => "write-file:selection:open".into(),
        other => other.to_owned(),
    }
}

fn region_text(client: &Client, view: phux_client_runtime::ViewId, region: TextRegion) -> String {
    let Some(engine) = client.engine() else { return String::new() };
    match engine.view_region_text_bounded(view, region, SEARCH_BYTES) {
        Ok(phux_client_runtime::engine::BoundedSelectionText::Text(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        _ => String::new(),
    }
}

fn short_path(path: Option<String>) -> String {
    let Some(path) = path else { return String::new() };
    if let Some(rest) = path.strip_prefix("/Users/").and_then(|rest| rest.split_once('/').map(|(_, rest)| rest)) {
        return format!("~/{rest}");
    }
    if let Some(rest) = path.strip_prefix("/home/").and_then(|rest| rest.split_once('/').map(|(_, rest)| rest)) {
        return format!("~/{rest}");
    }
    path
}

pub fn tab_title(desk: &Desk, tab: &crate::workspace::layout::DeskTab) -> String {
    if let Some(title) = tab.title.as_deref().filter(|title| !title.is_empty()) {
        return title.to_owned();
    }
    let focus = crate::workspace::layout::placements(&tab.root).into_iter().find(|placement| placement.id == tab.focused_id);
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
