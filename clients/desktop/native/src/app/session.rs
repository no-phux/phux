//! Process-wide windows, native menus, and the quick-terminal hotkey.

#[cfg(all(test, unix))]
mod journey_tests;
mod keys;
mod projects;
mod quick;
mod resume;
#[cfg(test)]
mod tests;
mod windows;

use std::path::PathBuf;

use gpui::{
    App, AppContext, Context, Entity, EntityId, KeyBinding, Menu, MenuItem, NoAction, Point,
    SharedString, Size, Subscription, Task, TitlebarOptions, WindowBounds, WindowHandle, WindowId,
    WindowOptions, px, size,
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
use super::notifications::SystemTag;

/// A divider in a [`MENU_BAR`] menu.
const MENU_SEPARATOR: &str = "-";

/// The menu bar after the app menu: each menu's catalog ids, in order.
/// AppKit appends its own list of open windows to the menu named "Window".
const MENU_BAR: &[(&str, &[&str])] = &[
    (
        "File",
        &[
            "new",
            "native-agent",
            "new-session",
            "new-window",
            "folder",
            MENU_SEPARATOR,
            "machines",
            "add-host",
            "reconnect",
            MENU_SEPARATOR,
            "close",
            "close-tab",
        ],
    ),
    (
        "Edit",
        &["copy", "paste", "select-all", MENU_SEPARATOR, "find"],
    ),
    (
        "View",
        &[
            "palette",
            "goto",
            MENU_SEPARATOR,
            "sidebar",
            "plain-terminal",
            "native-agents",
            "quick-terminal",
            MENU_SEPARATOR,
            "font-up",
            "font-down",
            "font-reset",
        ],
    ),
    (
        "Terminal",
        &[
            "split-right",
            "split-down",
            "view",
            "zoom",
            MENU_SEPARATOR,
            "clear",
            "attention",
            MENU_SEPARATOR,
            "terminate",
        ],
    ),
    ("Window", &["minimize", "windows"]),
];

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
    pub(crate) settings: Option<(
        AnyWindowHandle,
        Entity<super::settings_panel::window::SettingsWindow>,
    )>,
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
            // Off macOS, Root's Ctrl-C copies a conversation selection. In a
            // terminal Ctrl-C is the shell's interrupt, always.
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-c", NoAction, Some("PhuxTerminal")),
            KeyBinding::new("tab", OpenPathDirectory, Some("PathPicker > Input")),
        ]);
        let updates = cx.new(|cx| {
            let mut updater = super::updates::Updater::new();
            updater.watch_quit(cx);
            updater
        });
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
            settings: None,
            keymap: None,
            bound: Vec::new(),
            hotkey_chords: Vec::new(),
            hotkeys: None,
            _hotkey: None,
            _closed: None,
            fonts: false,
        });
        let updates = desktop.read(cx).updates.clone();
        updates.update(cx, |updater, _| {
            updater.desktop = Some(desktop.downgrade());
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
        self.launch_with_restore(options, false, cx)
    }

    pub(in crate::app) fn launch_with_restore(
        &mut self,
        options: AppOptions,
        restore: bool,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<WindowHandle<Root>> {
        self.phux = options.phux;
        self.socket = options.socket;
        self.home_session = options.session.clone();
        self.home_key = None;
        self.recent_projects = Some(RecentProjects::load(&self.socket.display().to_string()));
        self.listen_history_errors(cx);
        self.follow_layout(&options.session);
        // An explicit layout file is used as given: no legacy import beside it.
        let explicit = !options.layout_path.as_os_str().is_empty();
        self.explicit_layout = explicit;
        if explicit {
            self.layout = options.layout_path;
            self.legacy_layout = None;
            self.legacy_quick = None;
        }
        if !self.fonts {
            crate::fonts::install(cx);
            self.fonts = true;
        }
        let focus = crate::diag::may_activate();
        let resumed = restore
            .then(|| self.restore_update(focus, cx))
            .transpose()?
            .flatten();
        let handle = if let Some(handle) = resumed {
            handle
        } else if options.quick {
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
                fresh: options.fresh,
                initial_terminal: options.initial_terminal,
                layout_path: self.layout.clone(),
                legacy_layout: self.legacy_layout.clone(),
                stable_layout: !explicit,
                startup_error: options.startup_error,
                ..OpenRequest::default()
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

    /// Use the layout files of `session` on this socket.
    fn follow_layout(&mut self, session: &str) {
        let paths = crate::workspace::layout_paths(&self.socket.display().to_string(), session);
        self.layout = paths.layout;
        self.quick_layout = paths.quick;
        self.legacy_layout = Some(paths.legacy_layout);
        self.legacy_quick = Some(paths.legacy_quick);
    }

    /// Reopen the window `old` on `session`, for a launch that could not name
    /// a session: the home session and the layout files follow it.
    pub fn rehome(&mut self, old: EntityId, session: String, cx: &mut Context<Self>) {
        self.home_session = session.clone();
        self.home_key = None;
        if !self.explicit_layout {
            self.follow_layout(&session);
        }
        let quick = self
            .quick
            .as_ref()
            .is_some_and(|opened| opened.desk.entity_id() == old);
        let request = if quick {
            self.quick_request(cx)
        } else {
            OpenRequest {
                session,
                layout_path: self.layout.clone(),
                legacy_layout: self.legacy_layout.clone(),
                stable_layout: !self.explicit_layout,
                ..OpenRequest::default()
            }
        };
        let replaced = self
            .opened()
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
        self.open_fresh(session, None, remote, cx);
    }

    /// A secondary window that shows new terminals instead of a saved layout.
    fn open_fresh(
        &mut self,
        session: String,
        initial_terminal: Option<ResourceId>,
        remote: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let request = OpenRequest {
            role: WindowRole::Fresh,
            session,
            fresh: true,
            initial_terminal,
            layout_path: self.layout.clone(),
            remote,
            ..OpenRequest::default()
        };
        let _ = self.open_window(request, true, false, cx);
    }

    /// Every window, the quick one last.
    fn opened(&self) -> impl Iterator<Item = &Opened> {
        self.windows.iter().chain(self.quick.as_ref())
    }

    pub(crate) fn recent_projects(&self) -> &[PathBuf] {
        self.recent_projects
            .as_ref()
            .map_or(&[], RecentProjects::entries)
    }
}

/// Index of the first ready peer after `source` (wrapping) on the same
/// server. A source that is not listed (the quick window) starts from the
/// front.
fn next_peer<T: PartialEq + Copy>(
    peers: &[(T, Option<&str>, bool)],
    source: T,
    remote: Option<&str>,
) -> Option<usize> {
    let start = peers
        .iter()
        .position(|(id, ..)| *id == source)
        .map_or(0, |index| index + 1);
    (0..peers.len())
        .map(|step| (start + step) % peers.len())
        .find(|&index| {
            let (id, peer_remote, ready) = peers[index];
            id != source && peer_remote == remote && ready
        })
}

// Explicit Tab shortcuts must outrank the terminal's default traversal masks.
fn binding_context(stroke: &str) -> &'static str {
    match stroke {
        "tab" | "shift-tab" => "Desk || PhuxTerminal",
        // Outside a terminal these belong to text: Kit's fields and its
        // window root, which copies a selection in a conversation.
        "cmd-c" | "cmd-v" => "PhuxTerminal",
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
