//! The window's glass menus, built from the desk's state: the main menu
//! beside the window controls, the navigator's view menu, and the session,
//! tab and pane action menus. Each is a [`MenuSpec`] for the one
//! [`super::ui::menu`] component; this module only decides what they hold.
//!
//! One menu shows per window. Choosing an item runs the same catalog command
//! the palette, menu bar and key bindings run.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, Context, Point, SharedString, WeakEntity, Window, WindowHandle, point, px};
use gpui_kit::assets::IconName;

use super::actions::title;
use super::desk::{Desk, Modal, display_title};
use super::machine_name;
use super::ui::keystroke;
use super::ui::menu::{self, Anchor, Closed, GlassMenu, Heading, MenuItem, MenuPage, MenuSpec};
use super::view::group_work;
use crate::settings::SidebarGrouping;

mod pane;
mod view;
use pane::{pane_menu, terminal_menu};
use view::{view_menu, view_page};

/// A click that dismissed a menu by deactivating it arrives at its trigger
/// this soon after; it closes rather than reopens.
const REOPEN_GUARD: Duration = Duration::from_millis(400);

/// Trigger keys: what opened the showing menu, and where its anchor is.
pub(super) const MAIN: &str = "main-menu";
pub(super) const VIEW: &str = "sidebar-view";
pub(super) const HOST_FILTER: &str = "sidebar-host-filter";

pub(super) struct OpenMenu {
    key: SharedString,
    handle: WindowHandle<GlassMenu>,
}

impl Desk {
    /// Show the menu `spec` from `anchor`, or close it when the same trigger's
    /// menu is showing or has just closed.
    pub(super) fn toggle_menu(
        &mut self,
        key: impl Into<SharedString>,
        anchor: Anchor,
        mut spec: MenuSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = key.into();
        if let Some(open) = self.menu.take() {
            let showing = open
                .handle
                .read_with(cx, |menu, _| !menu.closing())
                .unwrap_or(false);
            if showing {
                let _ = open
                    .handle
                    .update(cx, |menu, window, cx| menu.dismiss(false, window, cx));
            }
            if open.key == key {
                return;
            }
        }
        if self
            .menu_closed
            .take()
            .is_some_and(|(closed, at)| closed == key && at.elapsed() < REOPEN_GUARD)
        {
            return;
        }
        let desk = cx.weak_entity();
        let closed = key.clone();
        let after = spec.on_close.take();
        spec.on_close = Some(Box::new(move |reason: Closed, cx: &mut App| {
            // The menu may close inside a desk update; settle afterwards.
            cx.defer(move |cx| {
                let _ = desk.update(cx, |desk, cx| {
                    if desk.menu.as_ref().is_some_and(|open| open.key == closed) {
                        desk.menu = None;
                    }
                    if reason == Closed::Away {
                        desk.menu_closed = Some((closed, Instant::now()));
                    }
                    cx.notify();
                });
                if let Some(after) = after {
                    after(reason, cx);
                }
            });
        }));
        let owner = cx.entity();
        if let Some(handle) = menu::open(spec, anchor, &owner, Desk::skin, window, cx) {
            self.menu = Some(OpenMenu { key, handle });
        }
    }

    /// Close the showing menu now, without restoring focus.
    pub(crate) fn close_menu(&mut self, cx: &mut App) {
        if let Some(open) = self.menu.take() {
            let _ = gpui::AnyWindowHandle::from(open.handle)
                .update(cx, |_, window, _| window.remove_window());
        }
    }

    /// The showing menu's window, for evidence and tests.
    pub fn menu_window(&self) -> Option<gpui::AnyWindowHandle> {
        self.menu.as_ref().map(|open| open.handle.into())
    }

    /// Whether the menu `key` opened is showing.
    pub(crate) fn menu_open(&self, key: &str) -> bool {
        self.menu.as_ref().is_some_and(|open| open.key == key)
    }

    /// Below the trigger painted for `key`, else at `fallback`.
    pub(super) fn anchor_below(&self, key: &str, fallback: Point<gpui::Pixels>) -> Anchor {
        self.anchors
            .get(key)
            .map_or(Anchor::At(fallback), Anchor::Below)
    }

    pub(crate) fn open_main_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let anchor = self.anchor_below(MAIN, point(px(78.), px(40.)));
        let spec = main_menu(self, cx);
        self.toggle_menu(MAIN, anchor, spec, window, cx);
    }

    pub(crate) fn open_view_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let anchor = self
            .anchors
            .get(VIEW)
            .map_or(Anchor::At(point(px(12.), px(80.))), Anchor::Above);
        let spec = view_menu(self, cx);
        self.toggle_menu(VIEW, anchor, spec, window, cx);
    }

    pub(crate) fn open_session_menu(
        &mut self,
        id: u32,
        name: &str,
        anchor: Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let spec = session_menu(self, id, name, cx);
        self.toggle_menu(format!("session-{id}"), anchor, spec, window, cx);
    }

    pub(crate) fn open_tab_menu(
        &mut self,
        tab: &str,
        anchor: Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let spec = tab_menu(self, tab, cx);
        self.toggle_menu(format!("tab-{tab}"), anchor, spec, window, cx);
    }

    /// The terminal's right-click menu, at the pointer.
    pub(crate) fn open_terminal_menu(
        &mut self,
        placement: &str,
        position: Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let spec = terminal_menu(self, placement, cx);
        self.toggle_menu(
            format!("terminal-{placement}"),
            Anchor::At(position),
            spec,
            window,
            cx,
        );
    }

    pub(crate) fn open_pane_menu(
        &mut self,
        placement: &str,
        anchor: Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut spec = pane_menu(self, placement, cx);
        let desk = cx.weak_entity();
        spec.on_close = Some(Box::new(move |_, cx| {
            let _ = desk.update(cx, |desk, cx| {
                desk.pane_menu_open = None;
                cx.notify();
            });
        }));
        let key = format!("pane-{placement}");
        self.toggle_menu(key.clone(), anchor, spec, window, cx);
        // The trigger stays mounted while its menu shows.
        self.pane_menu_open = self.menu_open(&key).then(|| placement.to_owned());
        cx.notify();
    }
}

/// Run `call` on the desk from a menu item.
fn on_desk(
    desk: &WeakEntity<Desk>,
    call: impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let desk = desk.clone();
    move |window, cx| {
        let _ = desk.update(cx, |desk, cx| call(desk, window, cx));
    }
}

/// The catalog command `id`, titled and chorded as everywhere else.
fn command(desk: &Desk, handle: &WeakEntity<Desk>, id: &'static str, icon: IconName) -> MenuItem {
    MenuItem::action(
        title(id),
        on_desk(handle, move |desk, window, cx| desk.execute(id, window, cx)),
    )
    .icon(icon)
    .chord(desk.bound_chord(id).and_then(keystroke))
    .selector(id)
}

fn spec(name: &str, root: MenuPage, width: f32, desk: &Desk) -> MenuSpec {
    MenuSpec {
        name: name.to_owned().into(),
        root,
        width,
        skin: desk.skin(),
        on_close: None,
    }
}

/// The session this window shows, the way to every other session, and what
/// to make next. Search lives in Go To and the palette, one item away.
fn main_menu(desk: &Desk, cx: &mut Context<Desk>) -> MenuSpec {
    let handle = cx.weak_entity();
    let machine = machine_name::label(desk.remote.as_deref());
    let session = if desk.session.is_empty() {
        "phux".to_owned()
    } else {
        desk.session.clone()
    };
    let mut items = vec![
        command(desk, &handle, "new", IconName::SquareTerminal),
        command(desk, &handle, "native-agent", IconName::Bot),
        command(desk, &handle, "new-session", IconName::FolderPlus),
        command(desk, &handle, "folder", IconName::FolderOpen),
    ];
    if !desk.recent_projects.is_empty() {
        items.push(
            MenuItem::page("Open Recent", recent_page(desk, &handle))
                .icon(IconName::Clock)
                .selector("recent"),
        );
    }
    items.extend([
        command(desk, &handle, "new-window", IconName::AppWindow),
        command(desk, &handle, "goto", IconName::ArrowRight).gap(),
        command(desk, &handle, "palette", IconName::Search),
        command(desk, &handle, "windows", IconName::Layers),
        MenuItem::page("View", view_page(desk, &handle))
            .icon(IconName::PanelLeft)
            .selector("view")
            .gap(),
        MenuItem::page("Pane", pane_page(desk, &handle))
            .icon(IconName::PanelsTopLeft)
            .selector("panes"),
        command(desk, &handle, "settings", IconName::Settings),
        command(desk, &handle, "reconnect", IconName::RefreshCw).gap(),
    ]);
    let root = MenuPage {
        heading: Some(Heading {
            title: session.into(),
            detail: Some(format!("On {machine} · Switch session").into()),
            icon: Some(machine_icon(desk.remote.as_deref())),
            page: Some(Rc::new(sessions_page(desk, &handle))),
        }),
        items,
    };
    spec("Main menu", root, 300., desk)
}

/// The mark for where a session runs: this Mac or another machine.
pub(super) fn machine_icon(host: Option<&str>) -> IconName {
    if host.is_some() {
        IconName::Server
    } else {
        IconName::LaptopMinimal
    }
}

/// Every session on this Mac and saved machines, project by project, each
/// marked with its machine; choosing one opens it in this window. Machines
/// themselves are managed one step further.
fn sessions_page(desk: &Desk, handle: &WeakEntity<Desk>) -> MenuPage {
    let mut items = Vec::new();
    for group in group_work(desk.work_catalog(), SidebarGrouping::Project) {
        for (index, work) in group.work.into_iter().enumerate() {
            let host = work.destination.host.clone();
            let mut words: Vec<String> = group.label.iter().cloned().collect();
            words.push(machine_name::label(host.as_deref()));
            if !work.online {
                words.push("offline".into());
            }
            let current = desk.work_is_active(&work.destination);
            let selector = format!("session-{}-{}", work.destination.host_label(), work.id);
            let target = work.destination.clone();
            let item = MenuItem::action(
                display_title(&work.destination.name),
                on_desk(handle, move |desk, _, cx| {
                    desk.navigate_work(target.clone(), cx)
                }),
            )
            .icon(machine_icon(host.as_deref()))
            .detail(words.join(" · "))
            .checked(current)
            .selector(selector);
            let starts_group = index == 0 && !items.is_empty();
            items.push(if starts_group { item.gap() } else { item });
        }
    }
    items.push(command(desk, handle, "machines", IconName::Settings).gap());
    items.push(command(desk, handle, "add-host", IconName::Plus));
    MenuPage {
        heading: Some(Heading {
            title: "Switch Session".into(),
            detail: Some("Sessions on every machine, by project".into()),
            ..Heading::default()
        }),
        items,
    }
}

fn recent_page(desk: &Desk, handle: &WeakEntity<Desk>) -> MenuPage {
    let mut items: Vec<_> = desk
        .recent_projects
        .iter()
        .map(|path| {
            let open = path.clone();
            MenuItem::action(
                folder_name(path),
                on_desk(handle, move |desk, window, cx| {
                    desk.open_project_path(open.clone(), window, cx)
                }),
            )
            .icon(IconName::Folder)
            .detail(path.display().to_string())
        })
        .collect();
    let forget = desk.recent_projects.clone();
    items.push(
        MenuItem::action(
            "Clear Recent",
            on_desk(handle, move |desk, _, cx| {
                for path in &forget {
                    desk.forget_recent_project(path, cx);
                }
            }),
        )
        .icon(IconName::Trash)
        .selector("clear-recent")
        .gap(),
    );
    MenuPage::titled("Open Recent", items)
}

fn folder_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn pane_page(desk: &Desk, handle: &WeakEntity<Desk>) -> MenuPage {
    MenuPage::titled(
        "Pane",
        vec![
            command(desk, handle, "split-right", IconName::PanelRight),
            command(desk, handle, "split-down", IconName::PanelBottom),
            command(desk, handle, "view", IconName::Copy),
            command(desk, handle, "zoom", IconName::Maximize),
        ],
    )
}

/// One tab's actions.
fn tab_menu(desk: &Desk, tab: &str, cx: &mut Context<Desk>) -> MenuSpec {
    let handle = cx.weak_entity();
    let (rename, close) = (tab.to_owned(), tab.to_owned());
    let items = vec![
        MenuItem::action(
            title("rename"),
            on_desk(&handle, move |desk, window, cx| {
                desk.open_modal(
                    Modal::Rename {
                        tab_id: rename.clone(),
                    },
                    window,
                    cx,
                )
            }),
        )
        .icon(IconName::Pencil)
        .selector("rename"),
        MenuItem::action(
            title("close-tab"),
            on_desk(&handle, move |desk, window, cx| {
                desk.close_tab(&close);
                desk.after_command(window, cx);
            }),
        )
        .icon(IconName::Close)
        .selector("close-tab")
        .gap(),
    ];
    spec("Tab actions", MenuPage::new(items), 240., desk)
}

/// A session's actions: new terminal there, organize, rename, kill.
fn session_menu(desk: &Desk, id: u32, name: &str, cx: &mut Context<Desk>) -> MenuSpec {
    let handle = cx.weak_entity();
    let Some(link) = desk.link.as_ref() else {
        return spec("Session actions", MenuPage::default(), 260., desk);
    };
    let connection = &link.connection;
    let session = connection
        .topology
        .as_ref()
        .and_then(|topology| topology.sessions.iter().find(|session| session.id == id));
    let designated = connection.is_intake_session(id);
    let intake = session
        .filter(|session| session.awaiting_organization && !designated)
        .and_then(|session| session.key);
    let project = connection
        .projects
        .get(&id)
        .and_then(|_| connection.directories.get(&id))
        .map(PathBuf::from);
    let mut items = vec![
        MenuItem::action(
            title("new"),
            on_desk(&handle, move |desk, window, cx| {
                desk.new_terminal_in(id, window, cx)
            }),
        )
        .icon(IconName::SquareTerminal)
        .selector("session-new"),
    ];
    if let Some(path) = project {
        items.push(
            MenuItem::action(
                "Open Project Window",
                on_desk(&handle, move |desk, window, cx| {
                    desk.open_project_path(path.clone(), window, cx)
                }),
            )
            .icon(IconName::FolderOpen)
            .selector("session-project"),
        );
    }
    if connection.organization() {
        items.push(
            MenuItem::action(
                title("select-session-work"),
                on_desk(&handle, move |desk, _, cx| desk.select_session_work(id, cx)),
            )
            .icon(IconName::ListTree)
            .selector("select-session-work")
            .gap(),
        );
    }
    if let Some(key) = session
        .and_then(|session| session.key)
        .filter(|_| connection.organization())
    {
        let current = connection.projects.get(&id).cloned();
        let name = name.to_owned();
        items.push(
            MenuItem::action(
                "Link to Project…",
                on_desk(&handle, move |desk, window, cx| {
                    desk.open_modal(
                        Modal::Project {
                            session: key,
                            name: name.clone(),
                            current: current.clone(),
                        },
                        window,
                        cx,
                    )
                }),
            )
            .icon(IconName::Link)
            .selector("link-project"),
        );
    }
    if let Some(key) = intake {
        let name = name.to_owned();
        items.push(
            MenuItem::action(
                "Mark Organized",
                on_desk(&handle, move |desk, _, cx| {
                    desk.resolve_intake(key, name.clone(), cx)
                }),
            )
            .icon(IconName::Check)
            .selector("mark-organized"),
        );
    }
    let rename_target = desk.rename_session_target(id);
    let current = name.to_owned();
    items.push(
        MenuItem::action(
            title("rename-session"),
            on_desk(&handle, move |desk, window, cx| {
                desk.open_modal(
                    Modal::Session {
                        create: false,
                        current: current.clone(),
                        target: rename_target.clone(),
                    },
                    window,
                    cx,
                )
            }),
        )
        .icon(IconName::Pencil)
        .selector("rename-session")
        .gap(),
    );
    if !designated {
        let target = desk.rename_session_target(id);
        let name = name.to_owned();
        items.push(
            MenuItem::action(
                title("kill-session"),
                on_desk(&handle, move |desk, window, cx| {
                    desk.open_modal(
                        Modal::KillSession {
                            name: name.clone(),
                            session: id,
                            target: target.clone(),
                        },
                        window,
                        cx,
                    )
                }),
            )
            .icon(IconName::Trash)
            .selector("kill-session")
            .danger(),
        );
    }
    spec("Session actions", MenuPage::new(items), 260., desk)
}

#[cfg(test)]
mod tests;
