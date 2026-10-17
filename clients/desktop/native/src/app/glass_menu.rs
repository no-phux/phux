//! A compositor material with a small, independently animated GPUI control tree.
//! The terminal window never participates in the menu's animation clock.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyWindowHandle, App, Bounds, Context, FocusHandle, FontWeight, Hsla,
    KeyDownEvent, Keystroke, MouseButton, ScrollHandle, SharedString, SpringAnimation,
    SpringConfig, SpringState, Subscription, WeakEntity, Window, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, px, size,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme, Icon};

#[cfg(any(test, feature = "ui-fixtures"))]
use crate::settings::skin::GLASS;
use crate::settings::{Backdrop, Skin, TabPosition};

use super::desk::{Desk, Modal};
use super::host::RemoteHost;
use super::view::keystroke;

const WIDTH: f32 = 316.;
const INSET: f32 = 8.;
const ROW: f32 = 40.;
/// Space above a row that starts a group.
const GAP: f32 = 8.;
const HEADER: f32 = 68.;
const HOME_HEIGHT: f32 = HEADER + INSET + ROW * 11. + 16.;

/// Text and icon ink, as foreground alpha: type hierarchy, not material.
const ICON: f32 = 0.8;
const SECONDARY: f32 = 0.55;
const MUTED: f32 = 0.5;
const HINT: f32 = 0.45;

/// The skin's menu spring.
fn spring(skin: &Skin) -> SpringConfig {
    let (stiffness, damping, mass) = skin.motion.spring;
    SpringConfig::new(stiffness, damping, mass)
}

/// How far through `span` `elapsed` is, 0 to 1. An empty span is done.
fn span_progress(elapsed: Duration, span: Duration) -> f32 {
    if span.is_zero() {
        return 1.;
    }
    (elapsed.as_secs_f32() / span.as_secs_f32()).clamp(0., 1.)
}

/// The catalogued command `id`.
fn builtin(id: &str) -> Option<&'static crate::settings::CommandSpec> {
    crate::settings::builtin_commands()
        .iter()
        .find(|command| command.id == id)
}

/// Whether the skin wants the compositor material behind the menu.
fn wants_glass(skin: &Skin) -> bool {
    skin.material.backdrop == Backdrop::Glass
}

/// Foreground washes on the menu, softer than the sheet's because the
/// material behind already reads as raised. They scale with the skin's
/// washes; the comments give the Liquid Glass values.
#[derive(Clone, Copy)]
struct Washes {
    /// Hovered or keyboard-selected header: 0.035.
    quiet: Hsla,
    /// The header's icon disc: 0.07.
    disc: Hsla,
    /// The sliding row selection: 0.085.
    selection: Hsla,
    /// The selection's hairline: 0.035.
    hairline: Hsla,
    /// A pressed row: 0.13.
    press: Hsla,
}

impl Washes {
    fn of(ink: Hsla, skin: &Skin) -> Self {
        let washes = skin.material.washes;
        Self {
            quiet: ink.opacity(washes.hover * 7. / 12.),
            disc: ink.opacity(washes.hover * 7. / 6.),
            selection: ink.opacity(washes.active * 17. / 22.),
            hairline: ink.opacity(washes.border * 7. / 16.),
            press: ink.opacity(washes.active * 13. / 11.),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Commands,
    Panes,
    Tabs,
    Hosts,
}

#[derive(Clone)]
enum Choice {
    Command(&'static str),
    Page(Page),
    Host(Option<String>),
    AddHost,
}

#[derive(Clone)]
struct Entry {
    label: SharedString,
    detail: Option<SharedString>,
    icon: IconName,
    choice: Choice,
    chord: Option<Keystroke>,
    checked: bool,
    gap: bool,
}

impl Entry {
    fn new(label: impl Into<SharedString>, icon: IconName, choice: Choice) -> Self {
        Self {
            label: label.into(),
            detail: None,
            icon,
            choice,
            chord: None,
            checked: false,
            gap: false,
        }
    }
}

struct SurfaceMotion {
    height: SpringState,
    target: f32,
    last: Instant,
}

impl SurfaceMotion {
    fn new(target: f32) -> Self {
        Self {
            height: SpringState {
                position: target - 18.,
                velocity: 0.,
            },
            target,
            last: Instant::now(),
        }
    }

    fn advance(&mut self, now: Instant, reduced: bool, spring: SpringConfig) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last)
            .as_secs_f32()
            .min(0.05);
        self.last = now;
        if reduced || spring.is_settled(self.height, self.target, 0.25) {
            self.height = SpringState {
                position: self.target,
                velocity: 0.,
            };
            return false;
        }
        self.height = spring.step(self.height, self.target, elapsed);
        true
    }
}

#[derive(Clone)]
struct MenuOwner {
    desk: WeakEntity<Desk>,
    parent: AnyWindowHandle,
    return_focus: Option<FocusHandle>,
}

struct MenuSnapshot {
    current: Option<String>,
    hosts: Vec<RemoteHost>,
    chords: Vec<(String, String)>,
    tab_position: TabPosition,
    skin: Skin,
}

#[cfg(any(test, feature = "ui-fixtures"))]
impl MenuSnapshot {
    /// A local menu with the default chords.
    fn fixture(skin: Skin, hosts: Vec<RemoteHost>) -> Self {
        let chords = crate::settings::default_chords();
        Self {
            current: None,
            hosts,
            chords: crate::settings::shown_chords(&chords, &chords),
            tab_position: TabPosition::Top,
            skin,
        }
    }
}

/// An ownerless menu in `window`, for previews and tests.
#[cfg(any(test, feature = "ui-fixtures"))]
fn fixture(
    snapshot: MenuSnapshot,
    max_height: f32,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Entity<GlassMenu> {
    cx.new(|cx| GlassMenu::new(snapshot, None, max_height, window, cx))
}

pub(super) struct GlassMenu {
    owner: Option<MenuOwner>,
    focus: FocusHandle,
    current: Option<String>,
    hosts: Vec<RemoteHost>,
    chords: Vec<(String, String)>,
    tab_position: TabPosition,
    page: Page,
    entries: Vec<Entry>,
    selected: Option<usize>,
    pointer: gpui::Point<gpui::Pixels>,
    scroll: ScrollHandle,
    max_height: f32,
    page_generation: usize,
    opened: Instant,
    closing: Option<Instant>,
    native_glass: bool,
    /// The skin painting now, kept in step with the desk while it lives.
    skin: Skin,
    surface: SurfaceMotion,
    _subscriptions: Vec<Subscription>,
}

pub(super) fn toggle(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    if let Some(handle) = desk.main_menu.take() {
        let dismissed = handle.update(cx, |menu, window, cx| {
            if menu.closing.is_some() {
                window.remove_window();
                false
            } else {
                menu.dismiss(true, window, cx);
                true
            }
        });
        if dismissed.unwrap_or(false) {
            return;
        }
    }
    let parent = window.window_handle();
    let return_focus = window.focused(cx);
    let bounds = window.bounds();
    let display = window.display(cx);
    let display_id = display.as_ref().map(|display| display.id());
    let screen = display.map(|display| display.bounds()).unwrap_or(bounds);
    let origin = point(
        (bounds.origin.x + px(78.))
            .min(screen.right() - px(WIDTH + 8.))
            .max(screen.origin.x + px(8.)),
        (bounds.origin.y + px(44.)).min(screen.bottom() - px(160.)),
    );
    let max_height = (screen.bottom() - origin.y - px(12.)).as_f32();
    let current = desk.remote.clone();
    let hosts = desk.hosts.clone();
    let chords = crate::settings::shown_chords(desk.keymap(), &crate::settings::default_chords());
    let tab_position = desk.prefs.tab_position;
    let skin = desk.skin();
    let owner = cx.entity();
    let weak = cx.weak_entity();
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                origin,
                size(px(WIDTH), px(HOME_HEIGHT - 18.).min(px(max_height))),
            ))),
            display_id,
            titlebar: None,
            focus: crate::diag::may_activate(),
            show: true,
            kind: WindowKind::PopUp,
            is_resizable: false,
            is_movable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        move |window, cx| {
            cx.new(|cx| {
                GlassMenu::new(
                    MenuSnapshot {
                        current,
                        hosts,
                        chords,
                        tab_position,
                        skin,
                    },
                    Some(MenuOwner {
                        desk: weak,
                        parent,
                        return_focus,
                    }),
                    max_height,
                    window,
                    cx,
                )
            })
        },
    );
    match result {
        Ok(handle) => {
            let close = move |cx: &mut App| {
                let _ =
                    AnyWindowHandle::from(handle).update(cx, |_, window, _| window.remove_window());
            };
            let entity = handle.entity(cx).expect("new menu root");
            entity.update(cx, |menu, cx| {
                menu._subscriptions.push(cx.observe_window_bounds(
                    window,
                    move |menu, parent, cx| {
                        // GPUI also reports bounds on activation. Opening this
                        // panel deactivates its parent without moving it.
                        if parent.bounds() == bounds {
                            return;
                        }
                        close(cx);
                        menu.closing = Some(Instant::now());
                    },
                ));
                menu._subscriptions.push(cx.observe_window_visibility(
                    window,
                    move |_, visibility, _, cx| {
                        if !visibility.is_visible() {
                            close(cx);
                        }
                    },
                ));
                // The desk notifies on every frame of a skin shift; follow it.
                menu._subscriptions
                    .push(cx.observe(&owner, |menu, desk, cx| {
                        let skin = desk.read(cx).skin();
                        if skin != menu.skin {
                            menu.skin = skin;
                            cx.notify();
                        }
                    }));
                menu._subscriptions
                    .push(cx.observe_release(&owner, move |_, _, cx| close(cx)));
            });
            desk.main_menu = Some(handle);
        }
        Err(error) => log::error!("opening glass menu: {error:#}"),
    }
}

impl GlassMenu {
    fn new(
        snapshot: MenuSnapshot,
        owner: Option<MenuOwner>,
        max_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A positive alpha keeps AppKit's drawable delivery alive during entry.
        window.set_window_opacity(0.01);
        let appearance = if cx.theme().is_dark() {
            WindowAppearance::Dark
        } else {
            WindowAppearance::Light
        };
        // Opaque skins paint their own solid surface; only clear ones get
        // the compositor material.
        let skin = snapshot.skin;
        let native_glass = wants_glass(&skin)
            && window.set_glass_effect(px(skin.shape.popover_radius), appearance);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut menu = Self {
            owner,
            focus,
            current: snapshot.current,
            hosts: snapshot.hosts,
            chords: snapshot.chords,
            tab_position: snapshot.tab_position,
            page: Page::Commands,
            entries: Vec::new(),
            selected: None,
            pointer: window.mouse_position(),
            scroll: ScrollHandle::new(),
            max_height,
            page_generation: 0,
            opened: Instant::now(),
            closing: None,
            native_glass,
            skin,
            surface: SurfaceMotion::new(HOME_HEIGHT.min(max_height)),
            _subscriptions: Vec::new(),
        };
        menu.entries = menu.entries_for(Page::Commands);
        if menu.owner.is_some() {
            menu._subscriptions
                .push(cx.observe_window_activation(window, |menu, window, cx| {
                    if !window.is_window_active() {
                        menu.dismiss(false, window, cx);
                    }
                }));
        }
        menu
    }

    fn command(&self, id: &'static str, icon: IconName) -> Entry {
        let spec = builtin(id).expect("menu command catalogue");
        let mut entry = Entry::new(spec.title, icon, Choice::Command(id));
        entry.chord = self
            .chords
            .iter()
            .find(|(command, _)| command == id)
            .and_then(|(_, chord)| keystroke(chord));
        entry
    }

    fn entries_for(&self, page: Page) -> Vec<Entry> {
        match page {
            Page::Commands => {
                vec![
                    self.command("new", IconName::SquareTerminal),
                    self.command("native-agent", IconName::Bot),
                    self.command("new-session", IconName::Folder),
                    self.command("folder", IconName::FolderOpen),
                    self.command("new-window", IconName::AppWindow),
                    Entry {
                        gap: true,
                        ..Entry::new(
                            "Pane Layout",
                            IconName::PanelsTopLeft,
                            Choice::Page(Page::Panes),
                        )
                    },
                    Entry::new(
                        "Tab Placement",
                        IconName::PanelLeft,
                        Choice::Page(Page::Tabs),
                    ),
                    self.command("goto", IconName::ArrowRight),
                    self.command("palette", IconName::Search),
                    self.command("settings", IconName::Settings),
                    Entry {
                        gap: true,
                        ..self.command("reconnect", IconName::RefreshCw)
                    },
                ]
            }
            Page::Panes => vec![
                self.command("split-right", IconName::PanelRight),
                self.command("split-down", IconName::PanelBottom),
                self.command("view", IconName::Copy),
            ],
            Page::Tabs => [
                ("tabs-top", IconName::PanelsTopLeft, TabPosition::Top),
                ("tabs-side", IconName::PanelLeft, TabPosition::Side),
            ]
            .into_iter()
            .map(|(id, icon, position)| {
                let mut entry = self.command(id, icon);
                entry.checked = self.tab_position == position;
                entry
            })
            .collect(),
            Page::Hosts => {
                let mut local = Entry::new("Local", IconName::LaptopMinimal, Choice::Host(None));
                local.detail = Some("On this Mac".into());
                local.checked = self.current.is_none();
                let mut entries = vec![local];
                entries.extend(self.hosts.iter().map(|host| {
                    let mut entry = Entry::new(
                        host.name.clone(),
                        IconName::Server,
                        Choice::Host(Some(host.name.clone())),
                    );
                    entry.detail = Some(host.endpoint.clone().into());
                    entry.checked = self.current.as_ref() == Some(&host.name);
                    entry
                }));
                let mut add = Entry::new("Add Host…", IconName::Plus, Choice::AddHost);
                add.gap = true;
                entries.push(add);
                entries
            }
        }
    }

    fn row_height(entry: &Entry) -> f32 {
        if entry.detail.is_some() { 54. } else { ROW }
    }

    fn gap(entry: &Entry) -> f32 {
        if entry.gap { GAP } else { 0. }
    }

    /// Height of rows `..end`, each with its gap.
    fn rows_height(&self, end: usize) -> f32 {
        self.entries[..end]
            .iter()
            .map(|entry| Self::gap(entry) + Self::row_height(entry))
            .sum()
    }

    fn row_y(&self, index: usize) -> f32 {
        self.rows_height(index) + Self::gap(&self.entries[index])
    }

    fn page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        self.page = page;
        self.entries = self.entries_for(page);
        self.selected = None;
        self.scroll = ScrollHandle::new();
        self.page_generation += 1;
        let height = HEADER + INSET + self.rows_height(self.entries.len());
        self.surface.target = height.min(self.max_height);
        cx.notify();
    }

    fn dismiss(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        self.closing = Some(Instant::now());
        // Occluded panels do not receive display frames. Cleanup must still
        // finish when the user switches Spaces or the app deactivates.
        let handle = window.window_handle();
        let exit = self.skin.motion.menu_exit();
        if cx.reduce_motion() {
            window.remove_window();
        } else {
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(exit).await;
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            })
            .detach();
        }
        if let Some(owner) = self.owner.as_ref().filter(|_| restore) {
            let focus = owner.return_focus.clone();
            let _ = owner.parent.update(cx, |_, window, cx| {
                window.activate_window();
                if let Some(focus) = focus {
                    window.focus(&focus, cx);
                }
            });
        }
        cx.notify();
    }

    fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        if let Choice::Page(page) = choice {
            self.page(page, cx);
            return;
        }
        self.dismiss(true, window, cx);
        let Some(owner) = self.owner.clone() else {
            return;
        };
        let desk = owner.desk;
        let _ = owner.parent.update(cx, move |_, window, cx| {
            let _ = desk.update(cx, |desk, cx| match choice {
                Choice::Command(id) => desk.execute(id, window, cx),
                Choice::Host(host) => desk.switch_host(host, cx),
                Choice::AddHost => desk.open_modal(Modal::AddHost, window, cx),
                Choice::Page(_) => {}
            });
        });
    }

    /// The header opens the host list from home and goes home from a page.
    fn header_click(&mut self, cx: &mut Context<Self>) {
        let home = self.page == Page::Commands;
        self.page(if home { Page::Hosts } else { Page::Commands }, cx);
    }

    fn move_selection(&mut self, up: bool, window: &Window, cx: &mut Context<Self>) {
        // Slot zero is the persistent header. All rows remain reachable even
        // after pointer selection, and keyboard users can always switch hosts.
        let count = self.entries.len() + 1;
        let slot = self.selected.map_or(0, |index| index + 1);
        let next = if up {
            (slot + count - 1) % count
        } else {
            (slot + 1) % count
        };
        self.selected = next.checked_sub(1);
        cx.notify();
        let Some(index) = self.selected else {
            return;
        };
        let top = self.row_y(index);
        let bottom = top + Self::row_height(&self.entries[index]);
        let height = window.viewport_size().height.as_f32() - HEADER - INSET;
        let mut offset = self.scroll.offset();
        if top < -offset.y.as_f32() {
            offset.y = px(-top);
        } else if bottom > height - offset.y.as_f32() {
            offset.y = px(height - bottom);
        }
        self.scroll.set_offset(offset);
        cx.notify();
    }

    fn activate_selection(
        &mut self,
        submenu_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.selected else {
            self.header_click(cx);
            return;
        };
        let choice = self.entries[index].choice.clone();
        if !submenu_only || matches!(choice, Choice::Page(_)) {
            self.choose(choice, window, cx);
        }
    }

    fn shortcut(&mut self, stroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let command = self
            .chords
            .iter()
            .find(|(_, chord)| keystroke(chord).as_ref() == Some(stroke))
            .and_then(|(id, _)| builtin(id));
        if let Some(command) = command {
            self.choose(Choice::Command(command.id), window, cx);
        }
    }

    fn hover_select(
        &mut self,
        selected: Option<usize>,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.closing.is_some() || self.pointer == position {
            return;
        }
        self.pointer = position;
        if self.selected != selected {
            self.selected = selected;
            cx.notify();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if self.closing.is_some() {
            return;
        }
        self.pointer = window.mouse_position();
        match event.keystroke.key.as_str() {
            "escape" => self.dismiss(true, window, cx),
            "left" if self.page != Page::Commands => self.page(Page::Commands, cx),
            "left" => {}
            "up" | "down" => self.move_selection(event.keystroke.key == "up", window, cx),
            "enter" | "space" => self.activate_selection(false, window, cx),
            "right" => self.activate_selection(true, window, cx),
            _ => self.shortcut(&event.keystroke, window, cx),
        }
    }

    fn opacity(&self, now: Instant, reduced: bool) -> (f32, bool) {
        if reduced {
            return (if self.closing.is_some() { 0. } else { 1. }, false);
        }
        let motion = &self.skin.motion;
        if let Some(at) = self.closing {
            let presence = 1. - span_progress(now.duration_since(at), motion.menu_exit());
            return (presence * presence, presence > 0.);
        }
        let progress = span_progress(now.duration_since(self.opened), motion.menu_enter());
        (1. - (1. - progress).powi(3), progress < 1.)
    }

    /// Called only during render, where GPUI permits animation frame requests.
    fn animate_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let reduced = cx.reduce_motion();
        let moving = self.surface.advance(now, reduced, spring(&self.skin));
        let height = self.surface.height.position;
        if (window.viewport_size().height.as_f32() - height).abs() > 0.5 {
            window.resize(size(px(WIDTH), px(height)));
        }
        let (alpha, fading) = self.opacity(now, reduced);
        if self.closing.is_some() && alpha <= 0. {
            window.remove_window();
            return;
        }
        window.set_window_opacity(alpha.max(0.01));
        if !reduced && (moving || fading) {
            window.request_animation_frame();
        }
    }

    fn header(&self, ink: Hsla, washes: Washes, cx: &mut Context<Self>) -> impl IntoElement {
        let home = self.page == Page::Commands;
        let (title, detail): (SharedString, _) = match self.page {
            Page::Commands => (
                self.current
                    .clone()
                    .unwrap_or_else(|| "Local".into())
                    .into(),
                "Switch host",
            ),
            Page::Panes => ("Pane Layout".into(), "Split or duplicate this terminal"),
            Page::Tabs => ("Tab Placement".into(), "Choose top or side workspace tabs"),
            Page::Hosts => ("Switch host".into(), "Local and saved hosts"),
        };
        let icon = match (home, self.current.is_some()) {
            (true, true) => IconName::Server,
            (true, false) => IconName::LaptopMinimal,
            (false, _) => IconName::ChevronLeft,
        };
        div()
            .id("glass-menu-header")
            .role(gpui::Role::MenuItem)
            .aria_label(if home { "Switch host" } else { "Back to menu" })
            .aria_selected(self.selected.is_none())
            .when(self.selected.is_none(), |el| {
                el.aria_active_descendant().bg(washes.quiet)
            })
            .on_mouse_move(cx.listener(|menu, event: &gpui::MouseMoveEvent, _, cx| {
                menu.hover_select(None, event.position, cx);
            }))
            .h(px(HEADER))
            .w_full()
            .px(px(16.))
            .flex()
            .items_center()
            .gap(px(12.))
            .cursor_pointer()
            .on_click(cx.listener(|menu, _, _, cx| menu.header_click(cx)))
            .child(
                div()
                    .size(px(34.))
                    .rounded_full()
                    .bg(washes.disc)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(icon).size(px(17.))),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(ink.opacity(SECONDARY))
                            .child(detail),
                    ),
            )
            .when(home, |el| {
                el.child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(14.))
                        .text_color(ink.opacity(MUTED)),
                )
            })
            .hover(move |style| style.bg(washes.quiet))
    }
}

impl Render for GlassMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.animate_surface(window, cx);
        let skin = self.skin;
        let ink = cx.theme().foreground;
        let washes = Washes::of(ink, &skin);
        let springs = spring(&skin);
        let radius = px(skin.shape.popover_radius);
        let row_radius = px(skin.shape.kit_radius_lg);
        let native = self.native_glass;
        let surface = if skin.material.backdrop == Backdrop::Opaque {
            cx.theme().popover.alpha(1.)
        } else {
            cx.theme().popover
        };
        let highlight = self.selected.map(|index| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .h(px(Self::row_height(&self.entries[index])))
                .rounded(row_radius)
                .bg(washes.selection)
                .border_1()
                .border_color(washes.hairline)
                .with_spring(
                    "selection",
                    SpringAnimation::new(springs)
                        .to(px(self.row_y(index)))
                        .with_epsilon(0.1),
                    |el, y| el.top(y),
                )
        });
        let rows = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let choice = entry.choice.clone();
                let selector = match &entry.choice {
                    Choice::Command(id) => Some(*id),
                    Choice::Page(Page::Panes) => Some("panes"),
                    Choice::Page(Page::Tabs) => Some("tabs"),
                    Choice::Host(None) => Some("local"),
                    _ => None,
                };
                div()
                    .id(("glass-menu-row", index))
                    .role(gpui::Role::MenuItem)
                    .aria_label(entry.label.clone())
                    .aria_selected(self.selected == Some(index))
                    .when(self.selected == Some(index), |el| {
                        el.aria_active_descendant()
                    })
                    .debug_selector(move || match selector {
                        Some(id) => format!("glass-menu-{id}"),
                        None => format!("glass-menu-row-{index}"),
                    })
                    .relative()
                    .flex_shrink_0()
                    .h(px(Self::row_height(entry)))
                    .mt(px(Self::gap(entry)))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .cursor_pointer()
                    .on_mouse_move(
                        cx.listener(move |menu, event: &gpui::MouseMoveEvent, _, cx| {
                            menu.hover_select(Some(index), event.position, cx);
                        }),
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |menu, _, window, cx| {
                        menu.choose(choice.clone(), window, cx)
                    }))
                    .active(move |style| style.bg(washes.press).rounded(row_radius))
                    .child(
                        Icon::new(entry.icon)
                            .size(px(17.))
                            .text_color(ink.opacity(ICON)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().truncate().child(entry.label.clone()))
                            .children(entry.detail.clone().map(|detail| {
                                div()
                                    .text_size(px(11.))
                                    .text_color(ink.opacity(MUTED))
                                    .truncate()
                                    .child(detail)
                            })),
                    )
                    .children(entry.chord.clone().map(|chord| {
                        div()
                            .text_color(ink.opacity(HINT))
                            .child(Kbd::new(chord).appearance(false))
                    }))
                    .when(matches!(entry.choice, Choice::Page(_)), |el| {
                        el.child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(13.))
                                .text_color(ink.opacity(HINT)),
                        )
                    })
                    .when(entry.checked, |el| {
                        el.child(div().text_color(cx.theme().primary).child("✓"))
                    })
            })
            .collect::<Vec<_>>();
        div()
            .id("glass-menu")
            .role(gpui::Role::Menu)
            .aria_label("Workspace menu")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .rounded(radius)
            .overflow_hidden()
            .text_size(px(13.))
            .text_color(ink)
            .when(!native, |el| {
                el.bg(surface).child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(radius)
                        .border_1()
                        .border_color(cx.theme().border),
                )
            })
            .on_key_down(cx.listener(Self::key))
            .on_action(
                cx.listener(|menu, action: &super::actions::Run, window, cx| {
                    if let Some(command) = builtin(&action.id) {
                        menu.choose(Choice::Command(command.id), window, cx);
                    }
                }),
            )
            .on_mouse_down_out(cx.listener(|menu, _, window, cx| menu.dismiss(true, window, cx)))
            .child(self.header(ink, washes, cx))
            .child(
                div()
                    .absolute()
                    .top(px(HEADER))
                    .bottom(px(INSET))
                    .left(px(INSET))
                    .right(px(INSET))
                    .id("glass-menu-scroll")
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .children(highlight)
                            .children(rows)
                            .with_spring(
                                ("page", self.page_generation),
                                SpringAnimation::new(springs)
                                    .to(px(0.))
                                    .from(px(10.))
                                    .with_epsilon(0.1),
                                |el, y| el.relative().top(y),
                            ),
                    ),
            )
    }
}

/// Standalone presentation of the production component; commands close it.
#[cfg(feature = "ui-fixtures")]
pub(super) fn preview(cx: &mut gpui::App) -> anyhow::Result<()> {
    // `PHUX_GLASS_PREVIEW_SKIN=terminal` previews the opaque skin.
    let skin =
        *crate::settings::skin_by_id(&std::env::var("PHUX_GLASS_PREVIEW_SKIN").unwrap_or_default());
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(WIDTH), px(HOME_HEIGHT)), cx)),
            kind: WindowKind::PopUp,
            titlebar: None,
            focus: false,
            show: true,
            is_resizable: false,
            is_movable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title(&format!("{} menu", skin.name));
            let hosts = vec![
                RemoteHost {
                    name: "Studio".into(),
                    endpoint: "ssh://studio.local".into(),
                },
                RemoteHost {
                    name: "Build server".into(),
                    endpoint: "quic://build.local:443".into(),
                },
            ];
            fixture(MenuSnapshot::fixture(skin, hosts), 700., window, cx)
        },
    )?;
    Ok(())
}

/// Synchronous creation cost with a warmed GPUI window; no visible windows or daemon.
#[cfg(feature = "ui-fixtures")]
pub(super) fn benchmark(cx: &mut gpui::App) -> anyhow::Result<()> {
    let options = || WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(20.), px(60.)),
            size(px(WIDTH), px(HOME_HEIGHT)),
        ))),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    // Match the production path: GPUI already owns a window and a Metal context.
    let primer = cx.open_window(options(), |_, cx| cx.new(|_| BenchmarkPrimer))?;
    let mut samples = Vec::new();
    for _ in 0..12 {
        let started = Instant::now();
        let menu = cx.open_window(options(), |window, cx| {
            fixture(MenuSnapshot::fixture(GLASS, Vec::new()), 700., window, cx)
        })?;
        samples.push(started.elapsed().as_micros());
        menu.update(cx, |_, window, _| window.remove_window())?;
    }
    primer.update(cx, |_, window, _| window.remove_window())?;
    let first = samples[0];
    let mut warm = samples[1..].to_vec();
    warm.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"metric":"menu_creation_us", "first":first,"warm_median":warm[warm.len()/2], "warm_max":warm.last(), "samples":samples, "scope":"hidden native window creation including initial GPUI draw; excludes compositor presentation"})
    );
    Ok(())
}

#[cfg(feature = "ui-fixtures")]
struct BenchmarkPrimer;

#[cfg(feature = "ui-fixtures")]
impl Render for BenchmarkPrimer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child("Menu performance fixture")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_surface_motion_settles_and_stops() {
        for fps in [30, 60, 120] {
            let mut motion = SurfaceMotion::new(HOME_HEIGHT);
            let start = motion.last;
            for frame in 1..=fps {
                if frame == fps / 6 {
                    motion.target = HEADER + ROW * 3. + INSET;
                }
                motion.advance(
                    start + Duration::from_secs_f32(frame as f32 / fps as f32),
                    false,
                    spring(&GLASS),
                );
            }
            assert_eq!(motion.height.position, motion.target);
            assert_eq!(motion.height.velocity, 0.);
            assert!(
                !motion.advance(start + Duration::from_secs(2), false, spring(&GLASS)),
                "settled surfaces request no frames"
            );
        }
    }

    #[test]
    fn glass_washes_keep_the_shipped_menu_ink() {
        let ink = gpui::white();
        let washes = Washes::of(ink, &GLASS);
        for (wash, alpha) in [
            (washes.quiet, 0.035),
            (washes.disc, 0.07),
            (washes.selection, 0.085),
            (washes.hairline, 0.035),
            (washes.press, 0.13),
        ] {
            assert!((wash.a - alpha).abs() < 1e-6, "{} != {alpha}", wash.a);
        }
        assert_eq!(GLASS.shape.popover_radius, 26.);
        assert_eq!(GLASS.motion.menu_enter(), Duration::from_millis(150));
        assert_eq!(GLASS.motion.menu_exit(), Duration::from_millis(100));
        assert_eq!(spring(&GLASS), SpringConfig::new(950., 48., 1.));
        let terminal = crate::settings::skin_by_id("terminal");
        assert!(wants_glass(&GLASS) && !wants_glass(terminal));
        assert_eq!(span_progress(Duration::from_millis(5), Duration::ZERO), 1.);
    }

    #[test]
    fn reduced_motion_resolves_surface_immediately() {
        let mut motion = SurfaceMotion::new(HOME_HEIGHT);
        motion.target = 228.;
        assert!(!motion.advance(Instant::now(), true, spring(&GLASS)));
        assert_eq!(motion.height.position, 228.);
        assert_eq!(motion.height.velocity, 0.);
    }

    #[gpui::test]
    fn dismissal_finishes_without_display_frames(cx: &mut gpui::TestAppContext) {
        let handle = cx.update(|cx| {
            gpui_kit::init(cx);
            cx.open_window(WindowOptions::default(), |window, cx| {
                fixture(MenuSnapshot::fixture(GLASS, Vec::new()), 700., window, cx)
            })
            .unwrap()
        });
        handle
            .update(cx, |menu, window, cx| menu.dismiss(false, window, cx))
            .unwrap();
        cx.run_until_parked();
        // Simulate a covered window: deliver no further render/display frames.
        cx.executor().advance_clock(GLASS.motion.menu_exit());
        cx.run_until_parked();
        assert!(handle.read_with(cx, |_, _| ()).is_err());
    }

    #[gpui::test]
    fn keyboard_reaches_header_and_scrolls_long_host_lists(cx: &mut gpui::TestAppContext) {
        let handle = cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(10.), px(10.)),
                        size(px(WIDTH), px(220.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    let hosts = (0..70)
                        .map(|index| RemoteHost {
                            name: format!("Host {index}"),
                            endpoint: "ssh://fixture.invalid".into(),
                        })
                        .collect();
                    fixture(MenuSnapshot::fixture(GLASS, hosts), 220., window, cx)
                },
            )
            .unwrap()
        });
        let cx = &mut gpui::VisualTestContext::from_window(handle.into(), cx);
        cx.simulate_keystrokes("down");
        handle
            .update(cx, |menu, _, cx| {
                menu.hover_select(None, menu.pointer, cx);
                assert_eq!(
                    menu.selected,
                    Some(0),
                    "stationary pointer preserves keyboard selection"
                );
                menu.hover_select(None, menu.pointer + point(px(1.), px(0.)), cx);
                assert_eq!(
                    menu.selected, None,
                    "pointer movement resumes hover selection"
                );
            })
            .unwrap();
        cx.simulate_keystrokes("down up enter");
        assert_eq!(
            handle.read_with(cx, |menu, _| menu.page).unwrap(),
            Page::Hosts
        );
        cx.simulate_keystrokes("up");
        let bounds = cx
            .debug_bounds("glass-menu-row-71")
            .expect("last host-page row");
        assert!(bounds.origin.y >= px(HEADER));
        assert!(
            bounds.bottom() <= px(220. - INSET),
            "bounds={bounds:?}, scroll={:?}",
            handle
                .read_with(cx, |menu, _| menu.scroll.offset())
                .unwrap()
        );
        cx.update(|window, cx| {
            assert_eq!(
                window.simulate_next_frame(cx),
                0,
                "reduced motion has no queued animation frames"
            )
        });
        // App shortcuts remain usable on child pages as well as Commands.
        cx.simulate_keystrokes("cmd-,");
        assert!(handle.read_with(cx, |_, _| ()).is_err());
    }
}
