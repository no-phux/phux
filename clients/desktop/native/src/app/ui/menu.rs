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
/// A row's padding inside its selection, and the icon column it starts with.
const PAD: f32 = 10.;
const ICON_COLUMN: f32 = 16.;
/// The header's icon disc.
const DISC: f32 = 30.;
/// The chevron glyph's blank right side at 13pt: trailing chevrons give it
/// back so their ink ends on the shortcuts' edge.
const CHEVRON_BEARING: f32 = 3.5;
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

mod open;
mod render;
mod rows;
mod state;

#[cfg(any(test, feature = "ui-fixtures"))]
mod fixtures;
#[cfg(test)]
mod tests;

#[cfg(feature = "ui-fixtures")]
pub(crate) use fixtures::{benchmark, preview};
#[cfg(test)]
use fixtures::{fixture, sample};
pub(crate) use open::open;
