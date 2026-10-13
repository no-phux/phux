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
use phux_client_runtime::control::{DeliveryOutcome, Event, PathAnswer, Status};
use phux_client_runtime::engine::{Scroll, TextRegion};
use phux_protocol::ResourceId;
use phux_protocol::ids::{OperationId, ResourceKey, SatelliteHost};

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
use super::overlays::InputSlot;
use super::panes::Panes;
use super::session::Desktop;
use super::settings_panel::SettingsControls;
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
    /// Spawn a new terminal when nothing is restored, instead of showing the home pane.
    pub fresh: bool,
    pub initial_terminal: Option<ResourceId>,
    pub layout_path: PathBuf,
    pub legacy_layout: Option<PathBuf>,
    pub startup_error: Option<String>,
    /// The main window's display, which a quick window borrows.
    pub main_display: Option<DisplayPrefs>,
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
    /// The agent state an agent toast reports, for its colour.
    pub agent_state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    None,
    Commands,
    Goto,
    Settings,
    Path,
    MoveResources,
    Rename {
        tab_id: String,
    },
    Session {
        create: bool,
        current: String,
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
    /// The parsed Ghostty config, read at open and on `reload-config` only.
    ghostty: settings::GhosttyConfig,
    /// Built-in chords with Ghostty's binds layered on while `ghostty_keys`.
    keymap: Vec<(String, String)>,
    pub modal: Modal,
    /// The modal whose controls were last built and focused.
    prepared: Modal,
    pub find: Option<FindState>,
    pub picker: Option<paths::PickerState>,
    pub toasts: Vec<Toast>,
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
    /// Split container bounds from the last prepaint, keyed by split id.
    pub(crate) split_bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// Horizontal tab centers from the last prepaint, in tab order.
    pub(crate) tab_centers: Rc<RefCell<Vec<f32>>>,
    viewport: gpui::Size<gpui::Pixels>,
    _wake: Option<Task<()>>,
    _clock: Option<Task<()>>,
    _subs: Vec<Subscription>,
    /// The list palette shared by commands, goto, sessions, and the path picker.
    pub(crate) palette_state: Option<Entity<CommandState>>,
    pub(crate) modal_inputs: Vec<(InputSlot, Entity<InputState>, Subscription)>,
    pub(crate) find_input: Option<(Entity<InputState>, Subscription)>,
    pub(crate) settings_controls: Option<SettingsControls>,
    /// `OpenRequest::fresh`, kept so a rebuilt link spawns the same way.
    open_fresh: bool,
    /// Sessions folded closed in the sidebar, by name.
    pub(crate) collapsed: std::collections::HashSet<String>,
    /// The titlebar's window-drag surface, created once.
    pub(crate) drag_region: Option<Entity<crate::chrome::DragRegion>>,
    /// The window title last written.
    window_title: String,
    closed: bool,
    /// Session creates awaiting their outcome, by operation, with the name.
    creates: HashMap<OperationId, String>,
    /// Session kills awaiting their outcome, by operation, with the name.
    kills: HashMap<OperationId, String>,
    pub(crate) organization: super::organization::Organization,
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
        start_x: f32,
        moved: bool,
    },
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
        let mut desk = Self {
            desktop,
            role: request.role,
            socket,
            session: request.session.clone(),
            phux,
            focus,
            prefs,
            ghostty_found: text.is_some(),
            keymap,
            ghostty,
            modal: Modal::None,
            prepared: Modal::None,
            find: None,
            picker: None,
            toasts: Vec::new(),
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
            split_bounds: Rc::default(),
            tab_centers: Rc::default(),
            viewport: gpui::size(gpui::px(1320.), gpui::px(840.)),
            _wake: None,
            _clock: None,
            _subs: Vec::new(),
            palette_state: None,
            modal_inputs: Vec::new(),
            find_input: None,
            settings_controls: None,
            open_fresh: request.fresh,
            collapsed: Default::default(),
            drag_region: None,
            window_title: String::new(),
            closed: false,
            creates: HashMap::new(),
            kills: HashMap::new(),
            organization: Default::default(),
        };
        if let Some(error) = request.startup_error {
            desk.toast(
                ToastKind::Error,
                "Could not start the phux server",
                Some(error),
            );
        }
        if blocked {
            desk.toast(
                ToastKind::Error,
                "Saved layout preserved",
                Some("This layout is invalid or from an unsupported version. Layout and settings changes will not be saved.".into()),
            );
        }
        desk.attach(request.initial_terminal, cx);
        desk.sync_bindings(cx);
        desk.apply_kit_theme(cx);
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
        if self.session.is_empty() {
            // The launch could not name a session; its reason is already shown.
            self.connect_error = Some("No session to open".into());
            cx.notify();
            return;
        }
        match Connection::connect(self.socket.clone(), self.session.clone()) {
            Ok(connection) => {
                let mut workspace = Workspace::new(
                    self.session.clone(),
                    WorkspaceOptions {
                        fresh: self.open_fresh,
                        initial_terminal: initial,
                    },
                );
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
                self.toast(
                    ToastKind::Error,
                    "Could not connect",
                    Some(error.to_string()),
                );
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
        let (events, answers, epoch) = {
            let Some(link) = self.link.as_mut() else {
                return;
            };
            (
                link.connection.drain(),
                link.connection.take_path_answers(),
                link.connection.connection_epoch(),
            )
        };
        crate::diag::DRAIN_EVENTS.add(events.len() as u64);
        if self.agent_epoch != Some(epoch)
            || events.iter().any(crate::connection::starts_connection)
        {
            self.agent_seen.clear();
            self.toasts.retain(|toast| toast.kind != ToastKind::Agent);
            self.agent_epoch = Some(epoch);
        }
        for event in &events {
            if crate::connection::starts_connection(event) {
                self.organization.connection_changed();
            }
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
        if self
            .find
            .as_ref()
            .is_some_and(|find| self.placement(find.placement_id.as_str()).is_none())
        {
            self.find = None;
            self.find_input = None;
        }
        if self.modal == Modal::Path {
            self.pin_picker();
        }
        self.persist_if_changed();
        self.ensure_clock(cx);
        cx.notify();
    }

    fn note_event(&mut self, event: &Event) {
        self.note_terminal_event(event);
        self.note_operation_event(event);
        self.note_input_event(event);
        if let Event::Detached { message, .. } = event {
            self.toast(ToastKind::Error, "Detached", Some(message.clone()));
        }
    }

    fn note_operation_event(&mut self, event: &Event) {
        if let Event::OrganizationOperation { operation, outcome } = event {
            self.note_organization(*operation, outcome);
        }
        self.note_kill_event(event);
        self.note_create_event(event);
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
                self.toast(ToastKind::Info, &format!("{title} ended"), Some(status));
            }
            self.agent_seen.remove(terminal_id);
            self.toasts
                .retain(|toast| toast.terminal.as_ref() != Some(terminal_id));
        }
    }

    fn note_kill_event(&mut self, event: &Event) {
        if let Event::OrganizationOperation { operation, outcome } = event
            && let Some(name) = self.kills.remove(operation)
        {
            match outcome {
                phux_client_core::org_ops::OrgOutcome::Committed(receipt) => {
                    let ended = receipt.members.unwrap_or(0);
                    let noun = if ended == 1 { "terminal" } else { "terminals" };
                    self.toast(
                        ToastKind::Success,
                        &format!("Killed {name}"),
                        Some(format!("{ended} {noun} ended")),
                    );
                }
                other => self.toast(
                    ToastKind::Error,
                    &format!("Could not kill {name}"),
                    Some(crate::connection::org::outcome_sentence(other)),
                ),
            }
        }
    }

    fn note_create_event(&mut self, event: &Event) {
        if let Event::OrganizationOperation { operation, outcome } = event
            && let Some(name) = self.creates.remove(operation)
        {
            if outcome.succeeded() {
                self.toast(ToastKind::Success, "Session created", Some(name));
            } else {
                self.toast(
                    ToastKind::Error,
                    "Could not create the session",
                    Some(format!(
                        "{name}: {}",
                        crate::connection::org::outcome_sentence(outcome)
                    )),
                );
            }
        }
    }

    fn note_input_event(&mut self, event: &Event) {
        if let Some(message) = crate::connection::refused_input(event) {
            self.toast(ToastKind::Error, "Input not delivered", Some(message));
        }
        if let Event::InputDelivery {
            outcome: DeliveryOutcome::Refused,
            message,
            ..
        } = event
            && crate::connection::refused_input(event).is_none()
            && !message.is_empty()
        {
            self.toast(
                ToastKind::Error,
                "Input not delivered",
                Some(message.clone()),
            );
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
        // An agent toast goes away with its agent's badge.
        self.toasts.retain(|toast| {
            toast.kind != ToastKind::Agent
                || toast
                    .terminal
                    .as_ref()
                    .is_some_and(|id| current.contains_key(id))
        });
        if !self.prefs.notifications {
            self.agent_seen = current
                .into_iter()
                .map(|(id, info)| (id, info.state))
                .collect();
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
            let body = format!(
                "{} · {}",
                self.pane_title(Some(id)),
                short_path(self.pane_cwd(Some(id)))
            );
            self.toast_agent(title, body, id.clone(), info.state.clone());
        }
        self.agent_seen = current
            .into_iter()
            .map(|(id, info)| (id, info.state))
            .collect();
    }

    fn take_notices(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        for notice in link.workspace.take_notices() {
            let kind = if notice.kind == "error" {
                ToastKind::Error
            } else {
                ToastKind::Info
            };
            self.toast(kind, &notice.title, notice.body.clone());
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
            ScrollLines(rows) => self.scroll_lines(rows as i64),
            PromptJump(prompts) => self.jump_prompt(prompts as i64),
        }
    }

    fn run_catalog(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "palette" => self.modal = Modal::Commands,
            "goto" => self.modal = Modal::Goto,
            "settings" => self.modal = Modal::Settings,
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
                }
            }
            "rename-session" => self.open_rename_session(),
            "kill-session" => self.ask_kill_session(),
            "folder" => self.open_folder(window, cx),
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
            "close" => {
                self.workspace_call(|link| link.workspace.close_pane(&link.connection, None))
            }
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
            "find-close" => self.close_find(),
            "follow" | "scroll-bottom" => self.follow(),
            "paste-cwd" => self.paste_cwd(),
            "terminate" => self.ask_terminate(),
            "attention" => self.next_attention(),
            "sidebar" => {
                self.update_prefs(|prefs| prefs.sidebar_visible = !prefs.sidebar_visible, cx)
            }
            "font-up" => self.update_prefs(|prefs| prefs.font_size += 1.0, cx),
            "font-down" => self.update_prefs(|prefs| prefs.font_size -= 1.0, cx),
            "font-reset" => {
                let size = DisplayPrefs::default().font_size;
                self.update_prefs(|prefs| prefs.font_size = size, cx);
            }
            "option-alt" => {
                self.update_prefs(|prefs| prefs.option_as_alt = !prefs.option_as_alt, cx)
            }
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

    pub fn app_chords(&self) -> Vec<String> {
        settings::app_chords(&self.keymap)
    }

    pub fn shown_chord(&self, id: &str) -> Option<String> {
        settings::shown_chords(&self.keymap, &settings::default_chords())
            .into_iter()
            .find(|(command, _)| command == id)
            .map(|(_, chord)| settings::display_chord(&chord))
    }

    /// A modal, or the focused pane's find bar, owns the keyboard.
    pub(crate) fn text_field_has_keys(&self) -> bool {
        settings::text_field_has_keys(
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
            self.close_find();
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    pub fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Modal::Path) {
            self.picker = None;
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
        if let Some(saved) = &self.last_saved
            && let Err(error) = self.store.write(&persist::layout_value(saved))
        {
            self.toast(
                ToastKind::Error,
                "Could not save desktop layout",
                Some(error.to_string()),
            );
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
        if self.last_saved.as_ref() != Some(&next) {
            self.persist();
        }
    }

    /// Save, then release every view and the connection. Runs once per window.
    pub fn prepare_close(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.persist();
        if let Some(link) = self.link.as_mut() {
            link.workspace.release_all(&link.connection);
            link.connection.close();
        }
        self.panes.clear();
        cx.notify();
    }

    pub(crate) fn reconnect(&mut self, cx: &mut Context<Self>) {
        if self
            .link
            .as_ref()
            .is_none_or(|link| link.connection.status != Status::Attached)
        {
            match host::ensure_server(&self.phux, &self.socket, &self.session) {
                // A launch that could not name a session reopens on the one
                // the server now offers, with that session's saved layout.
                Ok(name) if self.session.is_empty() => {
                    let old = cx.entity_id();
                    let desktop = self.desktop.clone();
                    cx.defer(move |cx| {
                        desktop.update(cx, |desktop, cx| desktop.rehome(old, name, cx));
                    });
                    return;
                }
                Ok(_) => {}
                Err(error) => self.toast(
                    ToastKind::Error,
                    "Could not start the phux server",
                    Some(error),
                ),
            }
        }
        self.persist();
        let snapshot = self.last_saved.clone();
        let closed = self
            .link
            .as_ref()
            .is_some_and(|link| matches!(link.connection.status, Status::Closed | Status::Failed));
        if closed || self.link.is_none() {
            if let Some(link) = self.link.as_mut() {
                link.workspace.release_all(&link.connection);
                link.connection.close();
            }
            self.panes.clear();
            self.agent_seen.clear();
            self.toasts.retain(|toast| toast.kind != ToastKind::Agent);
            self.find = None;
            self.find_input = None;
            self.attach(None, cx);
            if let Some(link) = self.link.as_mut() {
                link.workspace.restore(snapshot);
            }
        } else if let Some(link) = self.link.as_mut() {
            link.workspace.release_all(&link.connection);
            self.panes.clear();
            self.agent_seen.clear();
            self.toasts.retain(|toast| toast.kind != ToastKind::Agent);
            self.find = None;
            self.find_input = None;
            link.workspace.restore(snapshot);
            if let Err(error) = link.connection.reconnect() {
                self.toast(
                    ToastKind::Error,
                    "Could not reconnect",
                    Some(error.to_string()),
                );
            }
        }
        self.toast(
            ToastKind::Info,
            "Reconnecting",
            Some(self.socket.display().to_string()),
        );
        cx.notify();
    }

    pub(crate) fn reload_ghostty(&mut self, cx: &mut Context<Self>) {
        let text = settings::read_ghostty_config();
        self.ghostty_found = text.is_some();
        self.ghostty = settings::parse_ghostty(text.as_deref().unwrap_or(""));
        self.sync_bindings(cx);
        self.apply_kit_theme(cx);
        if text.is_none() {
            self.toast(ToastKind::Info, "No Ghostty config found", None);
        } else {
            self.toast(ToastKind::Success, "Ghostty config reloaded", None);
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
            let paths = prompt
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .unwrap_or_default();
            if let Some(path) = paths.into_iter().next() {
                let _ = desk.update(cx, |desk, cx| {
                    let cwd = path.display().to_string();
                    desk.workspace_call(|link| {
                        link.workspace
                            .new_terminal(&link.connection, Some(cwd.as_str()))
                    });
                    desk.take_notices();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn open_rename_session(&mut self) {
        let name = self
            .focused_pane()
            .map(|pane| pane.session_name.clone())
            .unwrap_or_default();
        if name.trim().is_empty() {
            self.toast(
                ToastKind::Error,
                "No session",
                Some("Focus a pane in the session you want to rename.".into()),
            );
            return;
        }
        self.modal = Modal::Session {
            create: false,
            current: name,
        };
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
    /// the dialog open with a toast. A create on an `ORGANIZATION` server is
    /// persistent and reports its outcome when the server answers.
    pub fn apply_session(
        &mut self,
        name: String,
        directory: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Modal::Session { create, current } = self.modal.clone() else {
            return;
        };
        let directory = directory.unwrap_or_default();
        if create
            && let Some(link) = self.link.as_ref()
            && link.connection.organization()
        {
            let submitted = match crate::workspace::create_session_error(&name, &directory) {
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
                    self.toast(
                        ToastKind::Error,
                        "Could not create the session",
                        Some(failure),
                    );
                    cx.notify();
                }
            }
            return;
        }
        let failure = if create {
            host::create_session(&self.phux, &self.socket, &name, &directory)
        } else {
            host::rename_session(&self.phux, &self.socket, &current, &name)
        };
        if let Some(failure) = failure {
            self.toast(
                ToastKind::Error,
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
        self.toast(
            ToastKind::Success,
            if create {
                "Session created"
            } else {
                "Session renamed"
            },
            Some(name.trim().to_owned()),
        );
        self.close_modal(window, cx);
        self.after_command(window, cx);
    }

    /// Ask to kill the focused pane's session.
    fn ask_kill_session(&mut self) {
        let Some((name, session)) = self
            .focused_pane()
            .map(|pane| (pane.session_name.clone(), pane.session_id))
            .filter(|(name, _)| !name.trim().is_empty())
        else {
            self.toast(
                ToastKind::Error,
                "No session",
                Some("Focus a pane in the session you want to kill.".into()),
            );
            return;
        };
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
        let Some(link) = self.link.as_ref() else {
            self.toast(
                ToastKind::Error,
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
        if let Some(failure) = host::kill_session(&self.phux, &self.socket, name) {
            self.toast(
                ToastKind::Error,
                "Could not kill the session",
                Some(failure),
            );
        } else {
            let _ = link.connection.refresh_topology();
            self.toast(ToastKind::Success, &format!("Killed {name}"), None);
        }
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
        self.desktop
            .update(cx, |desktop, cx| desktop.open_session(session, cx));
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
        let order = ["blocked", "done", "working"];
        let mut panes: Vec<_> = link
            .connection
            .panes()
            .iter()
            .filter(|pane| {
                link.connection
                    .agents
                    .get(&pane.terminal_id)
                    .is_some_and(|agent| order.contains(&agent.state.as_str()))
            })
            .cloned()
            .collect();
        panes.sort_by(|left, right| {
            let rank = |pane: &phux_client_runtime::control::PaneDescriptor| {
                link.connection
                    .agents
                    .get(&pane.terminal_id)
                    .map(|agent| {
                        order
                            .iter()
                            .position(|state| *state == agent.state)
                            .unwrap_or(9)
                    })
                    .unwrap_or(9)
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
            self.toast(ToastKind::Info, "No agent needs attention", None);
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
        {
            self.panes.request(&id, action, cx);
        }
    }

    fn send_text(&mut self, text: &str, _cx: &mut Context<Self>) {
        let Some(terminal) = self.focused_terminal() else {
            return;
        };
        let events = typed_keys(text);
        let client = self.client();
        let Some(client) = client else { return };
        let sent = client.with_control(|control| {
            events
                .into_iter()
                .all(|event| control.send_key(&terminal, event))
        });
        if !sent {
            self.toast(
                ToastKind::Error,
                "Could not send keys",
                Some("The terminal did not accept the input.".into()),
            );
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
        self.paste_text(&host::quote_dropped_paths(&[PathBuf::from(cwd)]));
    }

    /// Paste dropped paths into the pane they were dropped on.
    pub fn drop_paths(&mut self, placement_id: &str, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let Some(terminal) = self
            .placement(placement_id)
            .and_then(|placement| placement.terminal_id.clone())
        else {
            return;
        };
        self.paste_text_to(&terminal, &host::quote_dropped_paths(paths));
    }

    fn paste_text(&mut self, text: &str) -> bool {
        let Some(terminal) = self.focused_terminal() else {
            return false;
        };
        self.paste_text_to(&terminal, text)
    }

    /// Queue a bracketed paste. False, with a toast, when the terminal is not ready.
    fn paste_text_to(&mut self, terminal: &ResourceId, text: &str) -> bool {
        let Some(client) = self.client() else {
            return false;
        };
        let queued = client.with_control(|control| {
            if !control.input_ready(terminal) {
                return false;
            }
            let _ = control.apply_paste(terminal, text);
            true
        });
        if queued {
            client.wake();
        } else {
            self.toast(
                ToastKind::Error,
                "Could not paste",
                Some("The terminal is not ready.".into()),
            );
        }
        queued
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
            self.toast(
                ToastKind::Info,
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
                self.toast(ToastKind::Error, "Could not write the file", Some(error));
                return;
            }
        };
        let shown = path.display().to_string();
        match mode {
            "paste" => {
                self.paste_text(&host::quote_dropped_paths(&[path]));
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
            self.run_search(query, case);
            if self.find_input.is_none() {
                self.find_input = Some(super::overlays::find_input(self, "", window, cx));
            }
            return;
        }
        self.close_find();
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
            self.close_find();
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
        self.run_search(query, case);
    }

    pub fn run_search(&mut self, query: String, case_sensitive: bool) {
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
        self.show_match(&placement, &matches, 0);
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
    }

    fn close_find(&mut self) {
        self.find_input = None;
        if let Some(state) = self.find.take()
            && let Some(placement) = self.placement(&state.placement_id).cloned()
            && let Some(view) = placement.view_id
            && let Some(engine) = self.client().and_then(|client| client.engine())
        {
            let _ = engine.clear_view_selection(view);
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
            self.toast(
                ToastKind::Info,
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
                "This server does not offer host path search. Update phux on the host."
            } else {
                "Not attached to a terminal."
            };
            self.toast(
                ToastKind::Info,
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
        let terminal = placement.terminal_id.clone()?;
        let colors = self.palette();
        let opacity = crate::settings::theme::with_alpha(
            &colors.theme.background,
            self.prefs.terminal_opacity,
        );
        let mut settings = TerminalSettings {
            font: gpui::font(self.prefs.font_family.clone()),
            font_size: self.prefs.font_size as f32,
            line_height: self.prefs.line_height as f32,
            cell_width_scale: self.prefs.cell_width as f32,
            cell_height_scale: self.prefs.cell_height as f32,
            foreground: Some(super::color::from_hex(&colors.theme.foreground)),
            background: Some(super::color::from_hex(&opacity)),
            cursor: Some(super::color::from_hex(&colors.theme.cursor)),
            selection_background: super::color::from_hex(&colors.theme.selection),
            selection_foreground: super::color::from_hex(
                colors
                    .theme
                    .selection_foreground
                    .as_deref()
                    .unwrap_or(&colors.theme.foreground),
            ),
            ..TerminalSettings::default()
        };
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
        settings.size_owner = self
            .link
            .as_ref()
            .is_some_and(|link| link.workspace.size_owner(placement));
        settings.app_chords = self.app_chords();
        let _ = terminal;
        Some(settings)
    }

    pub fn palette(&self) -> crate::settings::theme::Palette {
        let extra = settings::ghostty_theme(&self.ghostty);
        let extras = extra.as_slice();
        let theme = crate::settings::theme::theme_by_id(&self.prefs.theme_id, extras);
        crate::settings::theme::palette(&theme)
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        self.link
            .as_ref()
            .map(|link| link.connection.client.clone())
    }

    fn with_view(&self, call: impl FnOnce(&Client, phux_client_runtime::ViewId)) {
        let Some(view) = self.focused_view() else {
            return;
        };
        let Some(client) = self.client() else { return };
        call(&client, view);
    }

    fn with_engine(
        &self,
        call: impl FnOnce(&phux_client_runtime::engine::EngineHandle, phux_client_runtime::ViewId),
    ) {
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
            return ("connection failed · click to retry".into(), Tone::Bad);
        };
        match link.connection.status {
            Status::Attached => (
                format!(
                    "{} · {} {}",
                    self.session,
                    link.connection.panes().len(),
                    if link.connection.panes().len() == 1 {
                        "terminal"
                    } else {
                        "terminals"
                    }
                ),
                Tone::Good,
            ),
            Status::Idle | Status::Connecting | Status::Negotiated => {
                ("connecting…".into(), Tone::Busy)
            }
            Status::Failed => ("connection failed · click to retry".into(), Tone::Bad),
            Status::Closed => ("disconnected · click to reconnect".into(), Tone::Bad),
        }
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

    pub fn toast(&mut self, kind: ToastKind, title: &str, body: Option<String>) {
        self.toast_id += 1;
        self.toasts.push(Toast {
            id: self.toast_id,
            kind,
            title: title.to_owned(),
            body,
            at: Instant::now(),
            terminal: None,
            agent_state: None,
        });
        self.toasts = self
            .toasts
            .split_off(self.toasts.len().saturating_sub(TOAST_LIMIT));
    }

    fn toast_agent(&mut self, title: String, body: String, terminal: ResourceId, state: String) {
        self.toast_id += 1;
        self.toasts.push(Toast {
            id: self.toast_id,
            kind: ToastKind::Agent,
            title,
            body: Some(body),
            at: Instant::now(),
            terminal: Some(terminal),
            agent_state: Some(state),
        });
        self.toasts = self
            .toasts
            .split_off(self.toasts.len().saturating_sub(TOAST_LIMIT));
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|toast| toast.id != id);
    }

    /// Expire toasts (12 s for agent and error, 5 s otherwise) and keep agent ages
    /// current. Armed from every render; the task ends when nothing needs it.
    pub(crate) fn ensure_clock(&mut self, cx: &mut Context<Self>) {
        if self._clock.is_some() || !self.clock_needed() {
            return;
        }
        self._clock = Some(cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                let keep = this
                    .update(cx, |desk, cx| {
                        let now = Instant::now();
                        desk.toasts.retain(|toast| {
                            now.duration_since(toast.at) < toast_lifetime(toast.kind)
                        });
                        cx.notify();
                        let keep = desk.clock_needed();
                        if !keep {
                            desk._clock = None;
                        }
                        keep
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn clock_needed(&self) -> bool {
        !self.toasts.is_empty()
            || self
                .link
                .as_ref()
                .is_some_and(|link| !link.connection.agents.is_empty())
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
    }

    pub fn panes(&self) -> &Panes {
        &self.panes
    }

    pub fn panes_mut(&mut self) -> &mut Panes {
        &mut self.panes
    }

    /// `{tab} — phux`, or `phux` with no tab. Written only when it changes.
    pub(crate) fn sync_title(&mut self, window: &mut Window) {
        let title = self
            .active_tab()
            .map(|tab| format!("{} — phux", tab_title(self, tab)))
            .unwrap_or_else(|| "phux".into());
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
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

    pub fn begin_tab_drag(&mut self, id: String, x: f32, cx: &mut Context<Self>) {
        self.drag = Some(Drag::Tab {
            id,
            start_x: x,
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
            Some(Drag::Tab { id, start_x, moved }) => {
                if !*moved && (x - *start_x).abs() < 6. {
                    return;
                }
                *moved = true;
                let id = id.clone();
                let from = self
                    .link
                    .as_ref()
                    .and_then(|link| link.workspace.tabs.iter().position(|tab| tab.id == id));
                let Some(from) = from else { return };
                let to = tab_drop_index(from, &self.tab_centers.borrow(), x);
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
            link.workspace
                .new_terminal_in(&link.connection, session_id, None)
        });
        self.after_command(window, cx);
    }

    /// A terminal reports a click (focus its placement) or an OS file drop
    /// (paste into that pane).
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
            crate::terminal::TerminalEvent::PathsDropped(paths) => {
                self.drop_paths(placement_id, paths)
            }
        }
        cx.notify();
    }
}

/// Where a dragged tab lands: after the last other tab whose center is left of `x`.
pub(crate) fn tab_drop_index(from: usize, centers: &[f32], x: f32) -> usize {
    let mut to = 0;
    for (index, center) in centers.iter().enumerate() {
        if index != from && x > *center {
            to = if index < from { index + 1 } else { index };
        }
    }
    to
}

fn toast_lifetime(kind: ToastKind) -> Duration {
    match kind {
        ToastKind::Agent | ToastKind::Error => Duration::from_secs(12),
        ToastKind::Info | ToastKind::Success => Duration::from_secs(5),
    }
}

#[cfg(test)]
mod tests {
    use super::{pane_label, tab_drop_index};
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
