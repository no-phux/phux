//! Process-wide windows, native menus, and the quick-terminal hotkey.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, AppContext, Context, Entity, KeyBinding, Menu, MenuItem, Point, SharedString, Size, Task, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions, px, size,
};
use gpui_kit::component::Root;
use gpui_kit::{self, AnyWindowHandle};

use crate::hotkeys::GlobalHotkeys;
use crate::settings::{self, GhosttyConfig};
use phux_protocol::ResourceId;

use super::actions::{Quit, Run};
use super::commands::COMMANDS;
use super::desk::{Desk, OpenRequest, WindowRole};
use super::keys::gpui_stroke;
use super::AppOptions;

struct Opened {
    handle: AnyWindowHandle,
    desk: Entity<Desk>,
    role: WindowRole,
}

pub struct Desktop {
    phux: PathBuf,
    socket: PathBuf,
    home_session: String,
    layout: PathBuf,
    quick_layout: PathBuf,
    legacy_layout: Option<PathBuf>,
    legacy_quick: Option<PathBuf>,
    windows: Vec<Opened>,
    quick: Option<Opened>,
    hotkeys: Option<GlobalHotkeys>,
    _hotkey: Option<Task<()>>,
    fonts: bool,
}

impl Desktop {
    pub fn install(cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self {
            phux: PathBuf::new(),
            socket: PathBuf::new(),
            home_session: String::new(),
            layout: PathBuf::new(),
            quick_layout: PathBuf::new(),
            legacy_layout: None,
            legacy_quick: None,
            windows: Vec::new(),
            quick: None,
            hotkeys: None,
            _hotkey: None,
            fonts: false,
        })
    }

    pub fn launch(&mut self, options: AppOptions, cx: &mut Context<Self>) -> anyhow::Result<WindowHandle<Root>> {
        self.phux = options.phux;
        self.socket = options.socket;
        self.home_session = options.session.clone();
        let paths = settings::layout_paths(&self.socket, &options.session);
        self.layout = if options.layout_path.as_os_str().is_empty() { paths.layout } else { options.layout_path };
        self.quick_layout = paths.quick;
        self.legacy_layout = paths.legacy_layout;
        self.legacy_quick = paths.legacy_quick;
        if !self.fonts {
            crate::fonts::install(cx);
            self.fonts = true;
        }
        let role = if options.quick { WindowRole::Quick } else if options.fresh { WindowRole::Fresh } else { WindowRole::Main };
        let request = OpenRequest {
            role,
            session: options.session,
            fresh: options.fresh || options.quick,
            initial_terminal: options.initial_terminal,
            layout_path: if options.quick { self.quick_layout.clone() } else { self.layout.clone() },
            legacy_layout: if options.quick { self.legacy_quick.clone() } else { self.legacy_layout.clone() },
            startup_error: options.startup_error,
        };
        let focus = std::env::var_os("PHUX_DESKTOP_BACKGROUND").is_none();
        let handle = self.open_window(request, focus, options.quick, cx)?;
        self.arm_hotkeys(cx);
        self.publish_menus(cx);
        Ok(handle)
    }

    pub fn open_session(&mut self, session: String, cx: &mut Context<Self>) {
        let request = OpenRequest {
            role: WindowRole::Fresh,
            session,
            fresh: true,
            initial_terminal: None,
            layout_path: self.layout.clone(),
            legacy_layout: None,
            startup_error: None,
        };
        let _ = self.open_window(request, true, false, cx);
    }

    pub fn open_moved(&mut self, session: String, terminal: ResourceId, cx: &mut Context<Self>) {
        let request = OpenRequest {
            role: WindowRole::Fresh,
            session,
            fresh: true,
            initial_terminal: Some(terminal),
            layout_path: self.layout.clone(),
            legacy_layout: None,
            startup_error: None,
        };
        let _ = self.open_window(request, true, false, cx);
    }

    pub fn toggle_quick(&mut self, cx: &mut Context<Self>) {
        if let Some(opened) = self.quick.take() {
            opened.desk.update(cx, |desk, cx| desk.prepare_close(cx));
            let _ = opened.handle.update(cx, |_, window, _| window.remove_window());
            return;
        }
        let request = OpenRequest {
            role: WindowRole::Quick,
            session: self.home_session.clone(),
            fresh: false,
            initial_terminal: None,
            layout_path: self.quick_layout.clone(),
            legacy_layout: self.legacy_quick.clone(),
            startup_error: None,
        };
        let _ = self.open_window(request, true, true, cx);
    }

    pub fn quit(&mut self, cx: &mut Context<Self>) {
        let windows = std::mem::take(&mut self.windows);
        let quick = self.quick.take();
        for opened in windows.into_iter().chain(quick) {
            opened.desk.update(cx, |desk, cx| desk.prepare_close(cx));
        }
        cx.quit();
    }

    pub fn install_bindings(&mut self, ghostty: &GhosttyConfig, ghostty_keys: bool, cx: &mut Context<Self>) {
        cx.clear_key_bindings();
        gpui_kit::init(cx);
        let mut bindings = Vec::new();
        for (chord, id) in settings::effective_keymap(ghostty, ghostty_keys) {
            let Some(stroke) = gpui_stroke(&chord) else { continue };
            bindings.push(KeyBinding::new(&stroke, Run::new(id), Some("Desk")));
        }
        bindings.push(KeyBinding::new("cmd-q", Quit, Some("Desk")));
        cx.bind_keys(bindings);
        self.publish_menus(cx);
        if ghostty_keys {
            self.register_hotkeys(ghostty, cx);
        }
    }

    fn open_window(&mut self, request: OpenRequest, focus: bool, quick: bool, cx: &mut Context<Self>) -> anyhow::Result<WindowHandle<Root>> {
        let desktop = cx.entity();
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let bounds = if quick { size(px(1100.), px(460.)) } else { size(px(1320.), px(840.)) };
        let title: SharedString = if quick { "phux quick terminal".into() } else { "phux".into() };
        let (any, desk) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::centered(bounds, cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title),
                    appears_transparent: true,
                    traffic_light_position: Some(Point::new(px(14.), px(14.))),
                }),
                focus,
                show: true,
                window_min_size: Some(Size::new(px(640.), px(420.))),
                window_background: WindowBackgroundAppearance::Blurred,
                app_owns_titlebar_drag: true,
                ..WindowOptions::default()
            },
            cx,
            move |window, cx| {
                cx.new(|cx| Desk::open(desktop, phux, socket, request, window, cx))
            },
        )?;
        let handle = any.downcast::<Root>().ok_or_else(|| anyhow::anyhow!("window root was not gpui_kit::component::Root"))?;
        let opened = Opened { handle: any, desk, role: if quick { WindowRole::Quick } else { WindowRole::Main } };
        if quick {
            self.quick = Some(opened);
        } else {
            self.windows.push(opened);
        }
        Ok(handle)
    }

    fn publish_menus(&self, cx: &mut App) {
        let mut items = Vec::new();
        let mut group = "";
        for command in COMMANDS {
            if command.group != group {
                if !items.is_empty() {
                    items.push(MenuItem::separator());
                }
                group = command.group;
            }
            if is_numbered_tab(command.id) {
                continue;
            }
            let name = match self.chord(command.id) {
                Some(chord) => format!("{}    {chord}", command.title),
                None => command.title.to_owned(),
            };
            items.push(MenuItem::action(name, Run::new(command.id)));
        }
        cx.set_menus(vec![
            Menu::new("Phux").items([
                MenuItem::action("About Phux", Run::new("settings")),
                MenuItem::separator(),
                MenuItem::action("Settings…", Run::new("settings")),
                MenuItem::separator(),
                MenuItem::action("Quit Phux", Quit),
            ]),
            Menu::new("File").items([
                MenuItem::action("New Terminal", Run::new("new")),
                MenuItem::action("New Window", Run::new("new-window")),
                MenuItem::action("Open Folder…", Run::new("folder")),
                MenuItem::separator(),
                MenuItem::action("Close Pane", Run::new("close")),
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
            Menu::new("Terminal").items(items),
        ]);
    }

    fn chord(&self, _id: &str) -> Option<String> {
        None
    }

    fn register_hotkeys(&mut self, ghostty: &GhosttyConfig, cx: &mut Context<Self>) {
        let chords = settings::quick_terminal_chords(ghostty);
        if chords.is_empty() {
            self.hotkeys = None;
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
                let pressed = this.update(cx, |desktop, _| {
                    desktop.hotkeys.as_ref().is_some_and(|hotkeys| !hotkeys.take_pressed().is_empty())
                }).unwrap_or(false);
                if pressed {
                    let _ = this.update(cx, |desktop, cx| desktop.toggle_quick(cx));
                }
            }
        }));
    }

    fn arm_hotkeys(&mut self, cx: &mut Context<Self>) {
        let ghostty = settings::parse_ghostty(settings::read_ghostty_config().as_deref().unwrap_or(""));
        let enabled = self.windows.first().and_then(|opened| {
            opened.desk.read(cx).ok().map(|desk| desk.prefs.ghostty_keys)
        }).unwrap_or(true);
        if enabled {
            self.register_hotkeys(&ghostty, cx);
        }
    }
}

fn is_numbered_tab(id: &str) -> bool {
    id.starts_with("tab-") && id[4..].parse::<u8>().is_ok()
}

impl gpui::Render for Desktop {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl gpui::IntoElement {
        gpui::div()
    }
}
