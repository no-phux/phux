//! Process-wide windows, native menus, and the quick-terminal hotkey.

#[cfg(test)]
mod journey_tests;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, AppContext, Context, Entity, EntityId, KeyBinding, Menu, MenuItem, NoAction, Point,
    SharedString, Size, Subscription, Task, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowId, WindowOptions, px, size,
};
use gpui_kit::component::Root;
use gpui_kit::{self, AnyWindowHandle};

use crate::hotkeys::GlobalHotkeys;
use crate::settings::{self, DisplayPrefs, GhosttyConfig};
use crate::workspace::persist::{self, FileLayoutStore, RecentProjects};
use phux_protocol::ResourceId;
use phux_protocol::ids::SessionKey;

use super::AppOptions;
use super::actions::{OpenPathDirectory, Quit, Run};
use super::desk::{Desk, OpenRequest, WindowRole};
use super::keys::gpui_stroke;

struct Opened {
    handle: AnyWindowHandle,
    desk: Entity<Desk>,
    session: String,
    role: WindowRole,
    remote: Option<String>,
}

pub struct Desktop {
    pub(crate) updates: Entity<super::updates::Updater>,
    phux: PathBuf,
    socket: PathBuf,
    home_session: String,
    home_key: Option<SessionKey>,
    layout: PathBuf,
    /// The launch named its layout file; rehoming keeps it.
    explicit_layout: bool,
    quick_layout: PathBuf,
    legacy_layout: Option<PathBuf>,
    legacy_quick: Option<PathBuf>,
    recent_projects: Option<RecentProjects>,
    _history_errors: Option<Task<()>>,
    _quit: Option<Subscription>,
    windows: Vec<Opened>,
    quick: Option<Opened>,
    performance: Option<AnyWindowHandle>,
    /// The keymap last installed as GPUI bindings, so an unchanged keymap is not rebound.
    keymap: Option<Vec<(String, String)>>,
    /// GPUI strokes bound to [`Run`]; a stroke a later keymap drops is masked with `NoAction`.
    bound: Vec<String>,
    hotkey_chords: Vec<String>,
    hotkeys: Option<GlobalHotkeys>,
    _hotkey: Option<Task<()>>,
    _closed: Option<Subscription>,
    fonts: bool,
}

impl Desktop {
    /// Create the process singleton. Quit is global so it works with no focused window,
    /// and every window close goes through [`Self::window_closed`].
    pub fn install(cx: &mut App) -> Entity<Self> {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            // Root's focus traversal runs before bubbling terminal input.
            KeyBinding::new("tab", NoAction, Some("PhuxTerminal")),
            KeyBinding::new("shift-tab", NoAction, Some("PhuxTerminal")),
            KeyBinding::new("tab", OpenPathDirectory, Some("PathPicker > Input")),
        ]);
        let updates = cx.new(|_| super::updates::Updater::new());
        let desktop = cx.new(|_| Self {
            updates,
            phux: PathBuf::new(),
            socket: PathBuf::new(),
            home_session: String::new(),
            home_key: None,
            layout: PathBuf::new(),
            explicit_layout: false,
            quick_layout: PathBuf::new(),
            legacy_layout: None,
            legacy_quick: None,
            recent_projects: None,
            _history_errors: None,
            _quit: None,
            windows: Vec::new(),
            quick: None,
            performance: None,
            keymap: None,
            bound: Vec::new(),
            hotkey_chords: Vec::new(),
            hotkeys: None,
            _hotkey: None,
            _closed: None,
            fonts: false,
        });
        let weak = desktop.downgrade();
        // Deferred: a window can be removed from inside a `Desktop` update (quick toggle).
        let closed = cx.on_window_closed(move |cx, id| {
            let weak = weak.clone();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |desktop, cx| desktop.window_closed(id, cx));
            });
        });
        desktop.update(cx, |desktop, cx| {
            desktop._closed = Some(closed);
            desktop._quit = Some(cx.on_app_quit(|desktop, _| {
                if let Some(history) = &desktop.recent_projects {
                    for error in history.flush() {
                        log::error!("Could not save recent folders: {error}");
                    }
                }
                async {}
            }));
        });
        let weak = desktop.downgrade();
        cx.on_system_notification_response(move |response, cx| {
            let _ = weak.update(cx, |desktop, cx| {
                desktop.open_notification(&response.tag, cx)
            });
        });
        desktop
    }

    pub fn launch(
        &mut self,
        options: AppOptions,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<WindowHandle<Root>> {
        self.phux = options.phux;
        self.socket = options.socket;
        self.home_session = options.session.clone();
        self.home_key = None;
        let socket = self.socket.display().to_string();
        self.recent_projects = Some(RecentProjects::load(&socket));
        self.listen_history_errors(cx);
        let paths = crate::workspace::layout_paths(&socket, &options.session);
        // An explicit layout file is used as given: no legacy import beside it.
        let explicit = !options.layout_path.as_os_str().is_empty();
        self.explicit_layout = explicit;
        self.layout = if explicit {
            options.layout_path
        } else {
            paths.layout
        };
        self.quick_layout = paths.quick;
        self.legacy_layout = (!explicit).then_some(paths.legacy_layout);
        self.legacy_quick = (!explicit).then_some(paths.legacy_quick);
        if !self.fonts {
            crate::fonts::install(cx);
            self.fonts = true;
        }
        let focus = crate::diag::may_activate();
        let handle = if options.quick {
            let mut request = self.quick_request(cx);
            request.initial_terminal = options.initial_terminal;
            request.startup_error = options.startup_error;
            self.open_window(request, focus, true, cx)?
        } else {
            let request = OpenRequest {
                role: if options.fresh {
                    WindowRole::Fresh
                } else {
                    WindowRole::Main
                },
                session: options.session,
                home_key: None,
                fresh: options.fresh,
                initial_terminal: options.initial_terminal,
                layout_path: self.layout.clone(),
                legacy_layout: self.legacy_layout.clone(),
                stable_layout: !explicit,
                startup_error: options.startup_error,
                main_display: None,
                remote: None,
            };
            self.open_window(request, focus, false, cx)?
        };
        self.publish_menus(cx);
        self.updates.update(cx, |updater, cx| {
            updater.start(self.phux.clone(), self.socket.clone(), cx)
        });
        if options.dev_telemetry {
            self.open_performance(cx)?;
        }
        Ok(handle)
    }

    /// Reopen the window `old` on `session`, for a launch that could not name
    /// a session: the home session and the layout files follow it.
    pub fn rehome(&mut self, old: EntityId, session: String, cx: &mut Context<Self>) {
        self.home_session = session.clone();
        self.home_key = None;
        if !self.explicit_layout {
            let paths =
                crate::workspace::layout_paths(&self.socket.display().to_string(), &session);
            self.layout = paths.layout;
            self.quick_layout = paths.quick;
            self.legacy_layout = Some(paths.legacy_layout);
            self.legacy_quick = Some(paths.legacy_quick);
        }
        let quick = self
            .quick
            .as_ref()
            .is_some_and(|opened| opened.desk.entity_id() == old);
        let request = if quick {
            self.quick_request(cx)
        } else {
            OpenRequest {
                role: WindowRole::Main,
                session,
                home_key: None,
                fresh: false,
                initial_terminal: None,
                layout_path: self.layout.clone(),
                legacy_layout: self.legacy_layout.clone(),
                stable_layout: !self.explicit_layout,
                startup_error: None,
                main_display: None,
                remote: None,
            }
        };
        let replaced = self
            .windows
            .iter()
            .chain(self.quick.as_ref())
            .find(|opened| opened.desk.entity_id() == old)
            .map(|opened| opened.handle);
        if let Err(error) = self.open_window(request, true, quick, cx) {
            log::error!("could not reopen the window: {error:#}");
            return;
        }
        if let Some(handle) = replaced {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    pub fn open_session(
        &mut self,
        session: String,
        remote: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let request = OpenRequest {
            role: WindowRole::Fresh,
            session,
            home_key: None,
            fresh: true,
            initial_terminal: None,
            layout_path: self.layout.clone(),
            legacy_layout: None,
            stable_layout: false,
            startup_error: None,
            main_display: None,
            remote,
        };
        let _ = self.open_window(request, true, false, cx);
    }

    pub(crate) fn recent_projects(&self) -> &[PathBuf] {
        self.recent_projects
            .as_ref()
            .map_or(&[], RecentProjects::entries)
    }

    fn listen_history_errors(&mut self, cx: &mut Context<Self>) {
        let errors = self
            .recent_projects
            .as_ref()
            .expect("desktop launched")
            .errors();
        self._history_errors = Some(cx.spawn(async move |this, cx| {
            while let Ok(error) = errors.recv().await {
                let _ = this.update(cx, |desktop, cx| {
                    for opened in desktop.windows.iter().chain(desktop.quick.as_ref()) {
                        opened.desk.update(cx, |desk, cx| {
                            desk.post(
                                super::notifications::Kind::Error,
                                "Could not save recent folders",
                                Some(error.clone()),
                            );
                            cx.notify();
                        });
                    }
                });
            }
        }));
    }

    pub(crate) fn record_recent_project(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> std::io::Result<()> {
        self.recent_projects
            .as_mut()
            .expect("desktop launched")
            .record(path)?;
        self.publish_recent_projects(cx);
        Ok(())
    }

    pub(crate) fn forget_recent_project(
        &mut self,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> std::io::Result<()> {
        self.recent_projects
            .as_mut()
            .expect("desktop launched")
            .forget(path)?;
        self.publish_recent_projects(cx);
        Ok(())
    }

    /// A window may render while this entity is leased by `open_window`;
    /// publish the small snapshot after the current GPUI update completes.
    fn publish_recent_projects(&self, cx: &mut Context<Self>) {
        let paths = self.recent_projects().to_vec();
        let windows: Vec<_> = self
            .windows
            .iter()
            .chain(self.quick.as_ref())
            .map(|opened| opened.desk.downgrade())
            .collect();
        cx.defer(move |cx| {
            for window in windows {
                let _ = window.update(cx, |desk, cx| {
                    desk.recent_projects = paths.clone();
                    cx.notify();
                });
            }
        });
    }

    /// A folder is a durable session, not a new terminal in whichever
    /// session happened to be focused. Reuse its open window; otherwise load
    /// the layout scoped to this socket and session.
    pub fn open_project(
        &mut self,
        session: String,
        terminal: ResourceId,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if let Some(opened) = self.windows.iter().find(|opened| {
            opened.session == session && opened.role == WindowRole::Main && opened.remote.is_none()
        }) {
            cx.activate(true);
            opened
                .handle
                .update(cx, |_, window, _| window.activate_window())?;
            opened
                .desk
                .update(cx, |desk, cx| desk.show_project_terminal(terminal, cx));
            return Ok(());
        }
        let paths = crate::workspace::layout_paths(&self.socket.display().to_string(), &session);
        let request = OpenRequest {
            role: WindowRole::Main,
            session,
            home_key: None,
            fresh: false,
            initial_terminal: Some(terminal),
            layout_path: paths.layout,
            legacy_layout: None,
            stable_layout: true,
            startup_error: None,
            main_display: None,
            remote: None,
        };
        self.open_window(request, true, false, cx)?;
        Ok(())
    }

    /// Update reuse metadata without reading the desk while it is leased.
    pub(crate) fn sync_window_session(
        &mut self,
        desk: gpui::EntityId,
        name: &str,
        remote: Option<&str>,
        role: WindowRole,
    ) {
        if let Some(opened) = self
            .windows
            .iter_mut()
            .chain(self.quick.as_mut())
            .find(|opened| opened.desk.entity_id() == desk)
        {
            name.clone_into(&mut opened.session);
            opened.remote = remote.map(str::to_owned);
            opened.role = role;
        }
    }

    /// Any remaining window can keep the singleton home current after its own
    /// window closes. Once learned, the key prevents adopting a reused name.
    pub(crate) fn observe_topology(&mut self, topology: &phux_client_runtime::control::Topology) {
        let previous = self.home_key;
        crate::connection::sync_home_name(topology, &mut self.home_session, &mut self.home_key);
        if !self.explicit_layout
            && self.home_key != previous
            && let Some(key) = self.home_key
        {
            let paths = persist::session_layout_paths(&self.socket.display().to_string(), key);
            self.legacy_layout = Some(std::mem::replace(&mut self.layout, paths.layout));
            self.legacy_quick = Some(std::mem::replace(&mut self.quick_layout, paths.quick));
        }
    }

    pub fn open_moved(
        &mut self,
        session: String,
        terminal: ResourceId,
        remote: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let request = OpenRequest {
            role: WindowRole::Fresh,
            session,
            home_key: None,
            fresh: true,
            initial_terminal: Some(terminal),
            layout_path: self.layout.clone(),
            legacy_layout: None,
            stable_layout: false,
            startup_error: None,
            main_display: None,
            remote,
        };
        let _ = self.open_window(request, true, false, cx);
    }

    /// Show registered host `name`: raise its window, or find the session to
    /// open there off the main thread and open one. `done` hears the refusal.
    pub fn open_host(
        &mut self,
        name: String,
        done: impl FnOnce(Result<(), String>, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        if let Some(opened) = self
            .windows
            .iter()
            .find(|opened| opened.remote.as_deref() == Some(name.as_str()))
            && opened
                .handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            cx.activate(true);
            done(Ok(()), cx);
            return;
        }
        let probe = name.clone();
        cx.spawn(async move |this, cx| {
            let home =
                smol::unblock(move || crate::connection::home::open_remote_home(&probe)).await;
            let _ = this.update(cx, |desktop, cx| {
                let result = home.and_then(|session| {
                    let paths = crate::workspace::layout_paths(&format!("host:{name}"), &session);
                    let request = OpenRequest {
                        role: WindowRole::Main,
                        session,
                        home_key: None,
                        fresh: false,
                        initial_terminal: None,
                        layout_path: paths.layout,
                        legacy_layout: None,
                        stable_layout: true,
                        startup_error: None,
                        main_display: None,
                        remote: Some(name),
                    };
                    desktop
                        .open_window(request, true, false, cx)
                        .map(|_| ())
                        .map_err(|error| format!("{error:#}"))
                });
                done(result, cx);
            });
        })
        .detach();
    }

    /// Close the quick window if it is open, or open it. A handle whose window
    /// the user already closed counts as closed.
    pub fn toggle_quick(&mut self, cx: &mut Context<Self>) {
        if let Some(opened) = self.quick.take()
            && opened.handle.update(cx, |_, _, _| ()).is_ok()
        {
            opened.desk.update(cx, |desk, cx| desk.prepare_close(cx));
            let _ = opened
                .handle
                .update(cx, |_, window, _| window.remove_window());
            return;
        }
        if let Err(error) = self.resolve_quick_home(cx) {
            if let Some(opened) = self.windows.first() {
                opened.desk.update(cx, |desk, cx| {
                    desk.post(
                        super::notifications::Kind::Error,
                        "Could not open quick terminal",
                        Some(error),
                    );
                    cx.notify();
                });
            }
            return;
        }
        let request = self.quick_request(cx);
        let _ = self.open_window(request, true, true, cx);
    }

    fn resolve_quick_home(&mut self, cx: &App) -> Result<(), String> {
        let Some(key) = self.home_key else {
            return Ok(());
        };
        for opened in &self.windows {
            if opened.remote.is_some() {
                continue;
            }
            let desk = opened.desk.read(cx);
            let Some(link) = &desk.link else { continue };
            let replica = link.connection.client.organization();
            if replica.is_live() {
                let session = replica.session(&key).ok_or_else(|| "The original home session no longer exists. Open an existing session instead.".to_owned())?;
                self.home_session.clone_from(&session.name);
                return Ok(());
            }
        }
        Err("The home session's identity is unavailable. Reconnect a window before opening the quick terminal.".to_owned())
    }

    /// The quick window restores its own layout and spawns a fresh terminal only
    /// when nothing is saved. Its display is the main window's.
    fn quick_request(&self, cx: &App) -> OpenRequest {
        OpenRequest {
            role: WindowRole::Quick,
            session: self.home_session.clone(),
            home_key: self.home_key,
            fresh: true,
            initial_terminal: None,
            layout_path: self.quick_layout.clone(),
            legacy_layout: self.legacy_quick.clone(),
            stable_layout: !self.explicit_layout,
            startup_error: None,
            main_display: self.main_display(cx),
            remote: None,
        }
    }

    fn main_display(&self, cx: &App) -> Option<DisplayPrefs> {
        if let Some(opened) = self.windows.first() {
            return Some(opened.desk.read(cx).prefs.clone());
        }
        let store = FileLayoutStore::new(self.layout.clone(), self.legacy_layout.clone());
        persist::load_layout(&store.read())
            .initial
            .map(|layout| layout.display)
    }

    /// A system notification was clicked: bring its window forward and open the
    /// entry. The tag is `<desk entity id>/<entry id>`, as [`Desk::flush_system`] posts it.
    fn open_notification(&mut self, tag: &str, cx: &mut Context<Self>) {
        let Some((desk, entry)) = tag.split_once('/') else {
            return;
        };
        let (Ok(desk), Ok(entry)) = (desk.parse::<u64>(), entry.parse::<u64>()) else {
            return;
        };
        let Some(opened) = self
            .windows
            .iter()
            .chain(self.quick.iter())
            .find(|opened| opened.desk.entity_id().as_u64() == desk)
        else {
            return;
        };
        let target = opened.desk.clone();
        cx.activate(true);
        let _ = opened.handle.update(cx, |_, window, cx| {
            window.activate_window();
            target.update(cx, |desk, cx| desk.open_notification(entry, window, cx));
        });
    }

    /// Whether a performance window currently owns the live sampler.
    pub(crate) fn performance_is_open(&self) -> bool {
        self.performance.is_some()
    }

    pub(crate) fn open_performance(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        if let Some(handle) = self.performance
            && handle
                .update(cx, |_, window, _| {
                    if crate::diag::may_activate() {
                        window.activate_window();
                    }
                })
                .is_ok()
        {
            return Ok(());
        }
        self.performance = Some(super::performance::open(cx)?);
        Ok(())
    }

    pub(crate) fn stop_performance(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.performance.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn window_closed(&mut self, id: WindowId, cx: &mut Context<Self>) {
        if self
            .performance
            .is_some_and(|handle| handle.window_id() == id)
        {
            self.performance = None;
            return;
        }
        let closed = if let Some(index) = self
            .windows
            .iter()
            .position(|opened| opened.handle.window_id() == id)
        {
            Some(self.windows.remove(index))
        } else if self
            .quick
            .as_ref()
            .is_some_and(|opened| opened.handle.window_id() == id)
        {
            self.quick.take()
        } else {
            None
        };
        let Some(opened) = closed else { return };
        opened.desk.update(cx, |desk, cx| desk.prepare_close(cx));
        if self.windows.is_empty() && self.quick.is_none() {
            cx.quit();
        }
    }

    /// Bind `keymap` as GPUI key bindings in the `Desk` context, publish the menus
    /// (which show those bindings), and arm Ghostty's global quick-terminal chords.
    /// An unchanged keymap and chord set does nothing.
    pub fn install_bindings(
        &mut self,
        keymap: Vec<(String, String)>,
        ghostty: &GhosttyConfig,
        ghostty_keys: bool,
        cx: &mut Context<Self>,
    ) {
        let chords = if settings::register_global_hotkeys(Some(ghostty_keys)) {
            settings::quick_terminal_chords(ghostty)
        } else {
            Vec::new()
        };
        if chords != self.hotkey_chords {
            self.hotkey_chords = chords.clone();
            self.register_hotkeys(chords, cx);
        }
        if self.keymap.as_ref() == Some(&keymap) {
            return;
        }
        let strokes: Vec<(String, String)> = keymap
            .iter()
            .filter_map(|(chord, id)| gpui_stroke(chord).map(|stroke| (stroke, id.clone())))
            .collect();
        let mut bindings = Vec::new();
        for old in &self.bound {
            if !strokes.iter().any(|(stroke, _)| stroke == old) {
                bindings.push(KeyBinding::new(old, NoAction, Some(binding_context(old))));
            }
        }
        for (stroke, id) in &strokes {
            bindings.push(KeyBinding::new(
                stroke,
                Run::new(id.clone()),
                Some(binding_context(stroke)),
            ));
        }
        cx.bind_keys(bindings);
        self.bound = strokes.into_iter().map(|(stroke, _)| stroke).collect();
        self.keymap = Some(keymap);
        self.publish_menus(cx);
    }

    fn open_window(
        &mut self,
        request: OpenRequest,
        focus: bool,
        quick: bool,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<WindowHandle<Root>> {
        let desktop = cx.entity();
        let updates = self.updates.clone();
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let session = request.session.clone();
        let recent_projects = self.recent_projects().to_vec();
        let role = request.role;
        let remote = request.remote.clone();
        let bounds = if quick {
            size(px(1100.), px(460.))
        } else {
            size(px(1320.), px(840.))
        };
        let title: SharedString = if quick {
            "phux quick terminal".into()
        } else {
            "phux".into()
        };
        let (any, desk) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::centered(bounds, cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title),
                    appears_transparent: true,
                    traffic_light_position: Some(Point::new(px(14.), px(14.))),
                }),
                focus,
                kind: crate::diag::window_kind(),
                show: true,
                window_min_size: Some(Size::new(px(640.), px(420.))),
                window_background: WindowBackgroundAppearance::Blurred,
                app_owns_titlebar_drag: true,
                ..WindowOptions::default()
            },
            cx,
            move |window, cx| {
                cx.new(|cx| {
                    Desk::open(
                        desktop,
                        updates,
                        phux,
                        socket,
                        request,
                        recent_projects,
                        window,
                        cx,
                    )
                })
            },
        )?;
        let handle = any
            .downcast::<Root>()
            .ok_or_else(|| anyhow::anyhow!("window root was not gpui_kit::component::Root"))?;
        let opened = Opened {
            handle: any,
            desk,
            session,
            role,
            remote,
        };
        if quick {
            self.quick = Some(opened);
        } else {
            self.windows.push(opened);
        }
        Ok(handle)
    }

    /// Menu items name [`Run`] actions; the platform menu shows each one's bound
    /// chord. The palette holds the rest of the catalog.
    fn publish_menus(&self, cx: &mut App) {
        let menus = vec![
            Menu::new("Phux").items([
                MenuItem::action("Settings…", Run::new("settings")),
                MenuItem::separator(),
                MenuItem::action("Quit Phux", Quit),
            ]),
            Menu::new("File").items([
                MenuItem::action("New Terminal", Run::new("new")),
                MenuItem::action("New ACP Agent…", Run::new("native-agent")),
                MenuItem::action("New Session…", Run::new("new-session")),
                MenuItem::action("New Window", Run::new("new-window")),
                MenuItem::action("Open Folder…", Run::new("folder")),
                MenuItem::separator(),
                MenuItem::action("Close Pane", Run::new("close")),
                MenuItem::action("Close Tab", Run::new("close-tab")),
            ]),
            Menu::new("Edit").items([
                MenuItem::action("Copy", Run::new("copy")),
                MenuItem::action("Paste", Run::new("paste")),
                MenuItem::action("Select All", Run::new("select-all")),
                MenuItem::action("Find…", Run::new("find")),
            ]),
            Menu::new("View").items([
                MenuItem::action("Toggle Sidebar", Run::new("sidebar")),
                MenuItem::action("Command Palette", Run::new("palette")),
                MenuItem::action("Toggle Quick Terminal", Run::new("quick-terminal")),
            ]),
            Menu::new("Terminal").items([
                MenuItem::action("Split Right", Run::new("split-right")),
                MenuItem::action("Split Down", Run::new("split-down")),
                MenuItem::action("Zoom Pane", Run::new("zoom")),
                MenuItem::separator(),
                MenuItem::action("Clear Screen", Run::new("clear")),
                MenuItem::action("Jump to Agent Needing Attention", Run::new("attention")),
                MenuItem::separator(),
                MenuItem::action("Terminate Terminal Process…", Run::new("terminate")),
            ]),
        ];
        cx.set_menus(menus);
    }

    /// Replace the global quick-terminal chords. No chords drops the registration.
    fn register_hotkeys(&mut self, chords: Vec<String>, cx: &mut Context<Self>) {
        self.hotkeys = None;
        if chords.is_empty() {
            return;
        }
        let mut hotkeys = match GlobalHotkeys::new() {
            Ok(hotkeys) => hotkeys,
            Err(error) => {
                log::error!("global hotkeys unavailable: {error}");
                return;
            }
        };
        let mut registered = false;
        for chord in chords {
            match hotkeys.register(chord.clone()) {
                Ok(()) => registered = true,
                Err(error) => log::error!("global hotkey {chord} unavailable: {error}"),
            }
        }
        if !registered {
            return;
        }
        self.hotkeys = Some(hotkeys);
        if self._hotkey.is_some() {
            return;
        }
        self._hotkey = Some(cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(50)).await;
                let pressed = this
                    .update(cx, |desktop, _| {
                        desktop
                            .hotkeys
                            .as_ref()
                            .is_some_and(|hotkeys| !hotkeys.take_pressed().is_empty())
                    })
                    .unwrap_or(false);
                if pressed {
                    let _ = this.update(cx, |desktop, cx| desktop.toggle_quick(cx));
                }
            }
        }));
    }
}

// Explicit Tab shortcuts must outrank the terminal's default traversal masks.
fn binding_context(stroke: &str) -> &'static str {
    match stroke {
        "tab" | "shift-tab" => "Desk || PhuxTerminal",
        _ => "Desk",
    }
}

impl gpui::Render for Desktop {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}
