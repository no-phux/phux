//! The glass menu: every dropdown and context menu in the app.
//!
//! A menu is a [`MenuPage`] of [`MenuItem`]s, opened from an [`Anchor`] in a
//! small popup window of its own, so it wears the compositor's glass material
//! where the platform has one (macOS) and the skin's painted surface
//! elsewhere. Items run closures in the parent window or open nested pages;
//! the header can lead to a page too (the machine switcher) and goes back from
//! one. Keyboard: arrows move, Return/Space/Right choose, Left goes back,
//! Escape closes, and any item's chord chooses it. The menu animates on its
//! own clock; the terminal window never takes part in it.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyElement, AnyWindowHandle, App, Bounds, Context, Entity, FocusHandle,
    FontWeight, Hsla, KeyDownEvent, Keystroke, MouseButton, Pixels, Point, ScrollHandle,
    SharedString, SpringAnimation, SpringConfig, SpringState, Subscription, Window,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, div, point, px, size,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme, Icon};

use crate::settings::{Backdrop, Skin};

const INSET: f32 = 6.;
const ROW: f32 = 34.;
const DETAIL_ROW: f32 = 48.;
/// Space above a row that starts a group.
const GAP: f32 = 7.;
const HEADER: f32 = 56.;
/// Between the trigger and the menu.
const DROP: f32 = 6.;
/// The closest a menu comes to a screen edge.
const MARGIN: f32 = 8.;

/// Text and icon ink, as foreground alpha: type hierarchy, not material.
const ICON: f32 = 0.8;
const SECONDARY: f32 = 0.55;
const MUTED: f32 = 0.5;
const HINT: f32 = 0.45;

/// Something a chosen item does, in the window that opened the menu.
pub(crate) type Act = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) enum Choice {
    Run(Act),
    Page(Rc<MenuPage>),
}

#[derive(Clone)]
pub(crate) struct MenuItem {
    label: SharedString,
    detail: Option<SharedString>,
    icon: Option<IconName>,
    choice: Choice,
    chord: Option<Keystroke>,
    checked: bool,
    gap: bool,
    danger: bool,
    selector: Option<SharedString>,
}

impl MenuItem {
    pub(crate) fn new(label: impl Into<SharedString>, choice: Choice) -> Self {
        Self {
            label: label.into(),
            detail: None,
            icon: None,
            choice,
            chord: None,
            checked: false,
            gap: false,
            danger: false,
            selector: None,
        }
    }

    /// An item that runs `act` in the parent window.
    pub(crate) fn action(
        label: impl Into<SharedString>,
        act: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self::new(label, Choice::Run(Rc::new(act)))
    }

    /// An item that opens `page`.
    pub(crate) fn page(label: impl Into<SharedString>, page: MenuPage) -> Self {
        Self::new(label, Choice::Page(Rc::new(page)))
    }

    pub(crate) fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub(crate) fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(crate) fn chord(mut self, chord: Option<Keystroke>) -> Self {
        self.chord = chord;
        self
    }

    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Start a group: a little space above this item.
    pub(crate) fn gap(mut self) -> Self {
        self.gap = true;
        self
    }

    /// Ends or deletes something; reads in the theme's danger colour.
    pub(crate) fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// The row's debug selector, `glass-menu-{selector}`.
    pub(crate) fn selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.selector = Some(selector.into());
        self
    }

    fn height(&self) -> f32 {
        if self.detail.is_some() {
            DETAIL_ROW
        } else {
            ROW
        }
    }

    fn space(&self) -> f32 {
        if self.gap { GAP } else { 0. }
    }
}

/// A page's title area. On the first page it is optional and may lead to
/// another page; on a nested page it always shows, as the way back.
#[derive(Clone, Default)]
pub(crate) struct Heading {
    pub title: SharedString,
    pub detail: Option<SharedString>,
    pub icon: Option<IconName>,
    pub page: Option<Rc<MenuPage>>,
}

#[derive(Clone, Default)]
pub(crate) struct MenuPage {
    pub heading: Option<Heading>,
    pub items: Vec<MenuItem>,
}

impl MenuPage {
    pub(crate) fn new(items: Vec<MenuItem>) -> Self {
        Self {
            heading: None,
            items,
        }
    }

    pub(crate) fn titled(title: impl Into<SharedString>, items: Vec<MenuItem>) -> Self {
        Self {
            heading: Some(Heading {
                title: title.into(),
                ..Heading::default()
            }),
            items,
        }
    }

    fn rows_height(&self, end: usize) -> f32 {
        self.items[..end]
            .iter()
            .map(|item| item.space() + item.height())
            .sum()
    }

    fn row_y(&self, index: usize) -> f32 {
        self.rows_height(index) + self.items[index].space()
    }
}

/// Why a menu closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Closed {
    /// An item was chosen.
    Chosen,
    /// Escape, a click elsewhere, or the app or parent going away.
    Away,
}

/// Runs once when a menu starts closing.
pub(crate) type OnClose = Box<dyn FnOnce(Closed, &mut App)>;

/// What to open and how it looks.
pub(crate) struct MenuSpec {
    /// The accessible name.
    pub name: SharedString,
    pub root: MenuPage,
    pub width: f32,
    pub skin: Skin,
    /// Runs once when the menu starts closing, for whatever reason.
    pub on_close: Option<OnClose>,
}

/// Where a menu opens, in the parent window's coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Anchor {
    /// Dropped from a trigger, left edges aligned.
    Below(Bounds<Pixels>),
    /// Raised from a trigger near the bottom of the window, left edges aligned.
    Above(Bounds<Pixels>),
    /// At a point, as a context menu opens at the pointer.
    At(Point<Pixels>),
}

impl Anchor {
    fn rect(self) -> Bounds<Pixels> {
        match self {
            Self::Below(bounds) | Self::Above(bounds) => bounds,
            Self::At(at) => Bounds::new(at, size(px(0.), px(0.))),
        }
    }

    /// The menu's top-left corner in the parent window, for a menu `height`
    /// points tall.
    fn corner(self, height: f32) -> Point<Pixels> {
        match self {
            Self::Below(bounds) => point(bounds.left(), bounds.bottom() + px(DROP)),
            Self::Above(bounds) => point(bounds.left(), bounds.top() - px(DROP + height)),
            Self::At(at) => at,
        }
    }
}

/// Where a menu `width` wide that wants `height` lands on `screen` from
/// `anchor` in a parent at `parent`, and the most height it may take there.
fn place(
    anchor: Anchor,
    parent: Bounds<Pixels>,
    screen: Bounds<Pixels>,
    width: f32,
    height: f32,
) -> (Point<Pixels>, f32) {
    let x = (parent.origin.x + anchor.corner(height).x)
        .min(screen.right() - px(width + MARGIN))
        .max(screen.origin.x + px(MARGIN));
    if let Anchor::Above(trigger) = anchor {
        let bottom = parent.origin.y + trigger.top() - px(DROP);
        let room = (bottom - screen.origin.y - px(MARGIN)).as_f32();
        let height = height.min(room);
        return (point(x, bottom - px(height)), room);
    }
    let y = (parent.origin.y + anchor.corner(height).y).min(screen.bottom() - px(160.));
    (point(x, y), (screen.bottom() - y - px(12.)).as_f32())
}

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

struct SurfaceMotion {
    height: SpringState,
    target: f32,
    last: Instant,
}

impl SurfaceMotion {
    fn new(target: f32) -> Self {
        Self {
            height: SpringState {
                position: target - 14.,
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

pub(crate) struct GlassMenu {
    name: SharedString,
    parent: Option<AnyWindowHandle>,
    return_focus: Option<FocusHandle>,
    focus: FocusHandle,
    /// Pages from the first to the one showing.
    stack: Vec<Rc<MenuPage>>,
    selected: Option<usize>,
    pointer: Point<Pixels>,
    scroll: ScrollHandle,
    width: f32,
    max_height: f32,
    page_generation: usize,
    opened: Instant,
    closing: Option<Instant>,
    native_glass: bool,
    /// The skin painting now, kept in step with the owner while it lives.
    skin: Skin,
    surface: SurfaceMotion,
    /// Whether an item was chosen, for the close callback.
    chosen: bool,
    on_close: Option<OnClose>,
    _subscriptions: Vec<Subscription>,
}

/// The height `page` asks for.
fn page_height(page: &MenuPage, first: bool) -> f32 {
    header_height(page, first) + INSET * 2. + page.rows_height(page.items.len())
}

fn header_height(page: &MenuPage, first: bool) -> f32 {
    if page.heading.is_some() || !first {
        HEADER
    } else {
        0.
    }
}

/// Open `spec` from `anchor` in `window`. The menu follows `owner`'s skin
/// through `skin_of` and closes when the owner goes, the parent moves or
/// hides, or the app deactivates.
pub(crate) fn open<T: 'static>(
    spec: MenuSpec,
    anchor: Anchor,
    owner: &Entity<T>,
    skin_of: fn(&T) -> Skin,
    window: &mut Window,
    cx: &mut App,
) -> Option<WindowHandle<GlassMenu>> {
    let parent = window.window_handle();
    let return_focus = window.focused(cx);
    let bounds = window.bounds();
    let display = window.display(cx);
    let display_id = display.as_ref().map(|display| display.id());
    let screen = display.map(|display| display.bounds()).unwrap_or(bounds);
    let width = spec.width;
    let wanted = page_height(&spec.root, true);
    let (origin, max_height) = place(anchor, bounds, screen, width, wanted);
    let height = wanted.min(max_height);
    // The platform may refuse a window kind before building its view; the
    // spec waits in the cell for the next attempt.
    let spec = Rc::new(std::cell::RefCell::new(Some(spec)));
    let build = |kind: WindowKind, cx: &mut App| {
        let (spec, return_focus) = (spec.clone(), return_focus.clone());
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    origin,
                    size(px(width), px(height - 14.).max(px(ROW))),
                ))),
                display_id,
                titlebar: None,
                focus: crate::diag::may_activate(),
                show: true,
                kind,
                is_resizable: false,
                is_movable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            move |window, cx| {
                let spec = spec
                    .borrow_mut()
                    .take()
                    .expect("a menu window is built once");
                cx.new(|cx| {
                    GlassMenu::new(spec, Some((parent, return_focus)), max_height, window, cx)
                })
            },
        )
    };
    // A compositor that owns placement (Wayland) takes a parent-anchored
    // popup; the rest reject it and take an absolutely placed one.
    let (edge, gravity, drop) = match anchor {
        Anchor::Above(_) => (
            gpui::popup::PopupAnchor::TopLeft,
            gpui::popup::PopupGravity::TopRight,
            -DROP,
        ),
        _ => (
            gpui::popup::PopupAnchor::BottomLeft,
            gpui::popup::PopupGravity::BottomRight,
            DROP,
        ),
    };
    let anchored = WindowKind::AnchoredPopup(gpui::popup::PopupOptions {
        parent,
        anchor_rect: anchor.rect(),
        anchor: edge,
        gravity,
        constraint_adjustment: gpui::popup::PopupConstraintAdjustment::SLIDE_X
            | gpui::popup::PopupConstraintAdjustment::SLIDE_Y
            | gpui::popup::PopupConstraintAdjustment::FLIP_Y,
        offset: point(px(0.), px(drop)),
        grab: false,
    });
    let handle = match build(anchored, cx).or_else(|_| build(WindowKind::PopUp, cx)) {
        Ok(handle) => handle,
        Err(error) => {
            log::error!("opening a menu: {error:#}");
            return None;
        }
    };
    let close = move |cx: &mut App| {
        let _ = AnyWindowHandle::from(handle).update(cx, |_, window, _| window.remove_window());
    };
    let entity = handle.entity(cx).ok()?;
    entity.update(cx, |menu, cx| {
        menu._subscriptions
            .push(cx.observe_window_bounds(window, move |menu, parent, cx| {
                // GPUI also reports bounds on activation. Opening this panel
                // deactivates its parent without moving it.
                if parent.bounds() == bounds {
                    return;
                }
                menu.finish(cx);
                close(cx);
            }));
        menu._subscriptions.push(cx.observe_window_visibility(
            window,
            move |menu, visibility, _, cx| {
                if !visibility.is_visible() {
                    menu.finish(cx);
                    close(cx);
                }
            },
        ));
        // The owner notifies on every frame of a skin shift; follow it.
        menu._subscriptions
            .push(cx.observe(owner, move |menu, owner, cx| {
                let skin = skin_of(owner.read(cx));
                if skin != menu.skin {
                    menu.skin = skin;
                    cx.notify();
                }
            }));
        menu._subscriptions
            .push(cx.observe_release(owner, move |menu, _, cx| {
                menu.finish(cx);
                close(cx);
            }));
    });
    Some(handle)
}

impl GlassMenu {
    fn new(
        spec: MenuSpec,
        parent: Option<(AnyWindowHandle, Option<FocusHandle>)>,
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
        let skin = spec.skin;
        let native_glass = wants_glass(&skin)
            && window.set_glass_effect(px(skin.shape.popover_radius), appearance);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let owned = parent.is_some();
        let (parent, return_focus) = parent.unzip();
        let root = Rc::new(spec.root);
        let height = page_height(&root, true);
        let mut menu = Self {
            name: spec.name,
            parent,
            return_focus: return_focus.flatten(),
            focus,
            stack: vec![root],
            selected: None,
            pointer: window.mouse_position(),
            scroll: ScrollHandle::new(),
            width: spec.width,
            max_height,
            page_generation: 0,
            opened: Instant::now(),
            closing: None,
            native_glass,
            skin,
            surface: SurfaceMotion::new(height.min(max_height)),
            chosen: false,
            on_close: spec.on_close,
            _subscriptions: Vec::new(),
        };
        if owned {
            menu._subscriptions
                .push(cx.observe_window_activation(window, |menu, window, cx| {
                    if !window.is_window_active() {
                        menu.dismiss(false, window, cx);
                    }
                }));
        }
        cx.on_release(|menu, cx| menu.finish(cx)).detach();
        menu
    }

    fn page(&self) -> &MenuPage {
        self.stack.last().expect("a menu always has its first page")
    }

    fn first(&self) -> bool {
        self.stack.len() == 1
    }

    /// Whether the header takes clicks and a keyboard slot.
    fn header_acts(&self) -> bool {
        !self.first()
            || self
                .page()
                .heading
                .as_ref()
                .is_some_and(|heading| heading.page.is_some())
    }

    fn header_height(&self) -> f32 {
        header_height(self.page(), self.first())
    }

    fn show(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.scroll = ScrollHandle::new();
        self.page_generation += 1;
        self.surface.target = page_height(self.page(), self.first()).min(self.max_height);
        cx.notify();
    }

    fn push(&mut self, page: Rc<MenuPage>, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        self.stack.push(page);
        self.show(cx);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.closing.is_some() || self.first() {
            return;
        }
        self.stack.pop();
        self.show(cx);
    }

    /// Run the close callback, once.
    fn finish(&mut self, cx: &mut App) {
        if let Some(on_close) = self.on_close.take() {
            on_close(
                if self.chosen {
                    Closed::Chosen
                } else {
                    Closed::Away
                },
                cx,
            );
        }
    }

    pub(crate) fn dismiss(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        self.closing = Some(Instant::now());
        self.finish(cx);
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
        if let Some(parent) = self.parent.filter(|_| restore) {
            let focus = self.return_focus.clone();
            let _ = parent.update(cx, |_, window, cx| {
                window.activate_window();
                if let Some(focus) = focus {
                    window.focus(&focus, cx);
                }
            });
        }
        cx.notify();
    }

    /// Whether the menu is on its way out.
    pub(crate) fn closing(&self) -> bool {
        self.closing.is_some()
    }

    fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        let act = match choice {
            Choice::Page(page) => return self.push(page, cx),
            Choice::Run(act) => act,
        };
        self.chosen = true;
        self.dismiss(true, window, cx);
        if let Some(parent) = self.parent {
            let _ = parent.update(cx, move |_, window, cx| act(window, cx));
        }
    }

    /// The header opens its page from the first page and goes back from any other.
    fn header_click(&mut self, cx: &mut Context<Self>) {
        if !self.first() {
            return self.back(cx);
        }
        if let Some(page) = self
            .page()
            .heading
            .as_ref()
            .and_then(|heading| heading.page.clone())
        {
            self.push(page, cx);
        }
    }

    fn move_selection(&mut self, up: bool, window: &Window, cx: &mut Context<Self>) {
        // With an acting header, slot zero is the header, so keyboard users
        // can always switch pages.
        let header = usize::from(self.header_acts());
        let count = self.page().items.len() + header;
        if count == 0 {
            return;
        }
        let slot = self.selected.map_or(0, |index| index + header);
        let next = match (self.selected, header) {
            (None, 0) if up => count - 1,
            (None, 0) => 0,
            _ if up => (slot + count - 1) % count,
            _ => (slot + 1) % count,
        };
        self.selected = next.checked_sub(header);
        cx.notify();
        let Some(index) = self.selected else {
            return;
        };
        let page = self.page();
        let top = page.row_y(index);
        let bottom = top + page.items[index].height();
        let height = window.viewport_size().height.as_f32() - self.header_height() - INSET * 2.;
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
        pages_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.selected else {
            if self.header_acts() && !(pages_only && !self.first()) {
                self.header_click(cx);
            }
            return;
        };
        let choice = self.page().items[index].choice.clone();
        if !pages_only || matches!(choice, Choice::Page(_)) {
            self.choose(choice, window, cx);
        }
    }

    /// An item, on any page, whose chord is `stroke`.
    fn chorded(&self, stroke: &Keystroke) -> Option<Choice> {
        fn find(page: &MenuPage, stroke: &Keystroke) -> Option<Choice> {
            page.items.iter().find_map(|item| match &item.choice {
                _ if item.chord.as_ref() == Some(stroke) => Some(item.choice.clone()),
                Choice::Page(page) => find(page, stroke),
                Choice::Run(_) => None,
            })
        }
        find(&self.stack[0], stroke)
    }

    fn hover_select(
        &mut self,
        selected: Option<usize>,
        position: Point<Pixels>,
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
            "left" => self.back(cx),
            "up" | "down" => self.move_selection(event.keystroke.key == "up", window, cx),
            "enter" | "space" => self.activate_selection(false, window, cx),
            "right" => self.activate_selection(true, window, cx),
            _ => {
                if let Some(choice) = self.chorded(&event.keystroke) {
                    self.choose(choice, window, cx);
                }
            }
        }
    }

    /// An app action that reached the menu (a menu-bar command while the
    /// menu has focus) closes it and runs in the parent window.
    fn forward(
        &mut self,
        action: &super::super::actions::Run,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = action.clone();
        self.choose(
            Choice::Run(Rc::new(move |window, cx| {
                window.dispatch_action(Box::new(action.clone()), cx)
            })),
            window,
            cx,
        );
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
            window.resize(size(px(self.width), px(height)));
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

    fn header(&self, ink: Hsla, washes: Washes, cx: &mut Context<Self>) -> Option<AnyElement> {
        let first = self.first();
        let heading = self.page().heading.clone().unwrap_or_default();
        if first && self.page().heading.is_none() {
            return None;
        }
        let acts = self.header_acts();
        let leads = first && acts;
        let icon = if first {
            heading.icon
        } else {
            Some(IconName::ChevronLeft)
        };
        let selected = acts && self.selected.is_none();
        Some(
            div()
                .id("glass-menu-header")
                .debug_selector(|| "glass-menu-header".into())
                .role(gpui::Role::MenuItem)
                .aria_label(if first {
                    heading.title.clone()
                } else {
                    format!("Back from {}", heading.title).into()
                })
                .aria_selected(selected)
                .when(selected, |el| el.aria_active_descendant().bg(washes.quiet))
                .on_mouse_move(cx.listener(|menu, event: &gpui::MouseMoveEvent, _, cx| {
                    menu.hover_select(None, event.position, cx);
                }))
                .h(px(HEADER))
                .w_full()
                .px(px(14.))
                .flex()
                .items_center()
                .gap(px(10.))
                .when(acts, |el| {
                    el.cursor_pointer()
                        .on_click(cx.listener(|menu, _, _, cx| menu.header_click(cx)))
                        .hover(move |style| style.bg(washes.quiet))
                })
                .children(icon.map(|icon| {
                    div()
                        .flex_shrink_0()
                        .size(px(30.))
                        .rounded_full()
                        .bg(washes.disc)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(icon).size(px(15.)))
                }))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(heading.title),
                        )
                        .children(heading.detail.map(|detail| {
                            div()
                                .text_size(px(11.))
                                .text_color(ink.opacity(SECONDARY))
                                .truncate()
                                .child(detail)
                        })),
                )
                .when(leads, |el| {
                    el.child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(13.))
                            .text_color(ink.opacity(MUTED)),
                    )
                })
                .into_any_element(),
        )
    }

    fn row(
        &self,
        index: usize,
        item: &MenuItem,
        ink: Hsla,
        washes: Washes,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let choice = item.choice.clone();
        let row_radius = px(self.skin.shape.kit_radius_lg);
        let selector = item.selector.clone();
        let tone = if item.danger { cx.theme().danger } else { ink };
        div()
            .id(("glass-menu-row", index))
            .role(gpui::Role::MenuItem)
            .aria_label(item.label.clone())
            .aria_selected(self.selected == Some(index))
            .when(self.selected == Some(index), |el| {
                el.aria_active_descendant()
            })
            .debug_selector(move || match &selector {
                Some(id) => format!("glass-menu-{id}"),
                None => format!("glass-menu-row-{index}"),
            })
            .relative()
            .flex_shrink_0()
            .h(px(item.height()))
            .mt(px(item.space()))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .cursor_pointer()
            .text_color(tone)
            .on_mouse_move(
                cx.listener(move |menu, event: &gpui::MouseMoveEvent, _, cx| {
                    menu.hover_select(Some(index), event.position, cx);
                }),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(
                cx.listener(move |menu, _, window, cx| menu.choose(choice.clone(), window, cx)),
            )
            .active(move |style| style.bg(washes.press).rounded(row_radius))
            .child(
                div()
                    .w(px(16.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .children(
                        item.icon.map(|icon| {
                            Icon::new(icon).size(px(15.)).text_color(tone.opacity(ICON))
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().truncate().child(item.label.clone()))
                    .children(item.detail.clone().map(|detail| {
                        div()
                            .text_size(px(11.))
                            .text_color(ink.opacity(MUTED))
                            .truncate()
                            .child(detail)
                    })),
            )
            .children(item.chord.clone().map(|chord| {
                div()
                    .text_color(ink.opacity(HINT))
                    .child(Kbd::new(chord).appearance(false))
            }))
            .when(matches!(item.choice, Choice::Page(_)), |el| {
                el.child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(13.))
                        .text_color(ink.opacity(HINT)),
                )
            })
            .when(item.checked, |el| {
                el.child(div().text_color(cx.theme().primary).child("✓"))
            })
            .into_any_element()
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
        let page = self.stack.last().expect("first page").clone();
        let highlight = self.selected.map(|index| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .h(px(page.items[index].height()))
                .rounded(row_radius)
                .bg(washes.selection)
                .border_1()
                .border_color(washes.hairline)
                .with_spring(
                    "selection",
                    SpringAnimation::new(springs)
                        .to(px(page.row_y(index)))
                        .with_epsilon(0.1),
                    |el, y| el.top(y),
                )
        });
        let rows = page
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| self.row(index, item, ink, washes, cx))
            .collect::<Vec<_>>();
        let top = self.header_height() + INSET;
        let header = self.header(ink, washes, cx);
        div()
            .id("glass-menu")
            .role(gpui::Role::Menu)
            .aria_label(self.name.clone())
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
            .on_action(cx.listener(Self::forward))
            .on_mouse_down_out(cx.listener(|menu, _, window, cx| menu.dismiss(true, window, cx)))
            .children(header)
            .child(
                div()
                    .absolute()
                    .top(px(top))
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
                                    .from(px(8.))
                                    .with_epsilon(0.1),
                                |el, y| el.relative().top(y),
                            ),
                    ),
            )
    }
}

/// A sample menu with a header page and a long list, for previews and tests.
#[cfg(any(test, feature = "ui-fixtures"))]
fn sample(skin: Skin, hosts: usize) -> MenuSpec {
    let noop = || -> Act { Rc::new(|_, _| {}) };
    let machines = MenuPage::titled(
        "Switch Machine",
        (0..hosts.max(1))
            .map(|index| {
                MenuItem::new(format!("Host {index}"), Choice::Run(noop()))
                    .icon(IconName::Server)
                    .detail("ssh://fixture.invalid")
            })
            .collect(),
    );
    MenuSpec {
        name: "Fixture menu".into(),
        root: MenuPage {
            heading: Some(Heading {
                title: "main".into(),
                detail: Some("This Mac".into()),
                icon: Some(IconName::LaptopMinimal),
                page: Some(Rc::new(machines)),
            }),
            items: vec![
                MenuItem::new("New Terminal", Choice::Run(noop())).icon(IconName::SquareTerminal),
                MenuItem::new("New Agent…", Choice::Run(noop())).icon(IconName::Bot),
                MenuItem::page(
                    "View",
                    MenuPage::titled(
                        "View",
                        vec![MenuItem::new("Tabs on Top", Choice::Run(noop())).checked(true)],
                    ),
                )
                .icon(IconName::PanelLeft)
                .gap(),
                MenuItem::new("Settings…", Choice::Run(noop()))
                    .icon(IconName::Settings)
                    .chord(Keystroke::parse("cmd-,").ok())
                    .selector("settings"),
            ],
        },
        width: 280.,
        skin,
        on_close: None,
    }
}

/// An ownerless menu in `window`, for previews and tests.
#[cfg(any(test, feature = "ui-fixtures"))]
fn fixture(
    spec: MenuSpec,
    max_height: f32,
    window: &mut Window,
    cx: &mut App,
) -> Entity<GlassMenu> {
    cx.new(|cx| GlassMenu::new(spec, None, max_height, window, cx))
}

/// Standalone presentation of the production component.
#[cfg(feature = "ui-fixtures")]
pub(crate) fn preview(cx: &mut App) -> anyhow::Result<()> {
    // `PHUX_GLASS_PREVIEW_SKIN=terminal` previews the opaque skin.
    let skin =
        *crate::settings::skin_by_id(&std::env::var("PHUX_GLASS_PREVIEW_SKIN").unwrap_or_default());
    let spec = sample(skin, 2);
    let height = page_height(&spec.root, true);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(spec.width), px(height)), cx)),
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
            fixture(spec, 700., window, cx)
        },
    )?;
    Ok(())
}

/// Synchronous creation cost with a warmed GPUI window; no visible windows or daemon.
#[cfg(feature = "ui-fixtures")]
pub(crate) fn benchmark(cx: &mut App) -> anyhow::Result<()> {
    use crate::settings::skin::GLASS;
    let options = || WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(20.), px(60.)),
            size(px(280.), px(400.)),
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
            fixture(sample(GLASS, 0), 700., window, cx)
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
    use crate::settings::skin::GLASS;
    use std::cell::Cell;

    #[test]
    fn interrupted_surface_motion_settles_and_stops() {
        for fps in [30, 60, 120] {
            let mut motion = SurfaceMotion::new(400.);
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
        let mut motion = SurfaceMotion::new(400.);
        motion.target = 228.;
        assert!(!motion.advance(Instant::now(), true, spring(&GLASS)));
        assert_eq!(motion.height.position, 228.);
        assert_eq!(motion.height.velocity, 0.);
    }

    #[test]
    fn a_headless_first_page_reserves_no_header() {
        let page = MenuPage::new(vec![MenuItem::new("One", Choice::Run(Rc::new(|_, _| {})))]);
        assert_eq!(page_height(&page, true), INSET * 2. + ROW);
        assert_eq!(page_height(&page, false), HEADER + INSET * 2. + ROW);
        let trigger = Bounds::new(point(px(80.), px(600.)), size(px(120.), px(30.)));
        assert_eq!(
            Anchor::Below(trigger).corner(100.),
            point(px(80.), px(630. + DROP))
        );
        assert_eq!(
            Anchor::Above(trigger).corner(100.),
            point(px(80.), px(500. - DROP))
        );
        assert_eq!(
            Anchor::At(point(px(5.), px(9.))).corner(100.),
            point(px(5.), px(9.))
        );
        let screen = Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.)));
        let parent = Bounds::new(point(px(100.), px(50.)), size(px(800.), px(700.)));
        let (origin, room) = place(Anchor::Above(trigger), parent, screen, 260., 900.);
        assert_eq!(origin.y, px(MARGIN), "a tall menu stops at the screen top");
        assert_eq!(room, 650. - DROP - MARGIN);
        let (origin, _) = place(Anchor::Below(trigger), parent, screen, 260., 100.);
        assert_eq!(origin.x, px(180.));
    }

    #[gpui::test]
    fn dismissal_finishes_without_display_frames(cx: &mut gpui::TestAppContext) {
        let closed = Rc::new(Cell::new(0));
        let handle = cx.update(|cx| {
            gpui_kit::init(cx);
            let counted = closed.clone();
            cx.open_window(WindowOptions::default(), |window, cx| {
                let mut spec = sample(GLASS, 0);
                spec.on_close = Some(Box::new(move |closed, _| {
                    assert_eq!(closed, Closed::Away);
                    counted.set(counted.get() + 1)
                }));
                fixture(spec, 700., window, cx)
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
        assert_eq!(closed.get(), 1, "the close callback runs exactly once");
    }

    #[gpui::test]
    fn keyboard_reaches_header_and_scrolls_long_pages(cx: &mut gpui::TestAppContext) {
        let handle = cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(10.), px(10.)),
                        size(px(280.), px(220.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| fixture(sample(GLASS, 70), 220., window, cx),
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
        // Back to the header, then into its page.
        cx.simulate_keystrokes("down up enter");
        assert_eq!(handle.read_with(cx, |menu, _| menu.stack.len()).unwrap(), 2);
        cx.simulate_keystrokes("up");
        let bounds = cx
            .debug_bounds("glass-menu-row-69")
            .expect("last host-page row");
        assert!(bounds.origin.y >= px(HEADER));
        assert!(
            bounds.bottom() <= px(220. - INSET),
            "bounds={bounds:?}, scroll={:?}",
            handle
                .read_with(cx, |menu, _| menu.scroll.offset())
                .unwrap()
        );
        cx.simulate_keystrokes("left");
        assert_eq!(handle.read_with(cx, |menu, _| menu.stack.len()).unwrap(), 1);
        cx.update(|window, cx| {
            assert_eq!(
                window.simulate_next_frame(cx),
                0,
                "reduced motion has no queued animation frames"
            )
        });
        // An item's chord chooses it from any page.
        cx.simulate_keystrokes("cmd-,");
        assert!(handle.read_with(cx, |_, _| ()).is_err());
    }
}
