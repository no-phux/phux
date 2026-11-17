//! The window: a session sidebar, the terminal, and a status line.

use std::cell::RefCell;
use std::rc::Rc;

use rune::tok;
use rune::{Color, Cx, El, TextStyle, col, el, row};

use crate::desk::Desk;
use crate::desk::surfaces::Surface;
use crate::features::changes;
use crate::theme::{Marks, Selection, Theme};

// The lanes adopt these as they convert; until then only tests read some.
#[allow(dead_code, reason = "adopted by the converting lanes")]
mod badge;
#[allow(clippy::disallowed_methods)] // converted in the values commit
mod controls;
pub(crate) use controls::{
    Cover, Sheet, button, icon_press, keep_from_pane, keycap, lifted, pressable, tip,
};
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod documents;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod galaxy;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod hive;
mod icons;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod inbox;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod kitchen;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod machines;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod news;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod onboarding;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod overlay;
pub(crate) use overlay::key_hint;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod pair;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod palette;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod panes;
pub(crate) use panes::DIVIDER;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod phone;
pub(crate) use phone::edge_color as phone_edge_color;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod pitch;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod pitlane;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod playground;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod popup;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod questions;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod reef;
#[allow(dead_code, reason = "adopted by the converting lanes")]
pub(crate) mod rows;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod settings;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod sidebar;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod spatial;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod status;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod tabs;
#[allow(dead_code, reason = "adopted by the converting lanes")]
#[allow(clippy::disallowed_methods)] // owner path: the tokens' own raw values
pub(crate) mod tokens;
#[allow(clippy::disallowed_methods)] // unconverted: the owning lane removes this
mod updates;

pub(crate) use tokens::Tokens;

#[cfg(test)]
mod gallery_tests;
#[cfg(test)]
mod separator_tests;

/// The app chord's modifier as shown beside an action.
pub(crate) const MOD: &str = if cfg!(any(target_os = "macos", target_os = "ios")) {
    "⌘"
} else {
    "Ctrl+Shift+"
};
pub(crate) const SIDEBAR_W: f32 = 236.0;
/// The band the macOS traffic lights sit in when they draw over content,
/// and the titlebar Rune frames on Linux where the compositor draws none.
pub(crate) const TITLEBAR_H: f32 = 38.0;
/// Space between the terminal area's edge and its grid.
pub(crate) const PAD: f32 = 10.0;
/// The gap around and between the terminal cards, and how round they are.
const CARD_GAP: f32 = 8.0;
const CARD_RADIUS: f32 = 12.0;
/// How far a frosted surface blurs what it floats over, in points.
const FROST: f32 = 18.0;
#[allow(clippy::disallowed_methods)] // converted in the values commit
pub(crate) const LABEL: TextStyle = TextStyle::new(12.0, 400.0);
#[allow(clippy::disallowed_methods)] // converted in the values commit
pub(crate) const LABEL_STRONG: TextStyle = TextStyle::new(12.0, 600.0);
#[allow(clippy::disallowed_methods)] // converted in the values commit
pub(crate) const CAPTION: TextStyle = TextStyle::new(11.0, 500.0);
/// A sidebar row's name.
#[allow(clippy::disallowed_methods)] // converted in the values commit
pub(crate) const NAME: TextStyle = TextStyle::new(12.5, 500.0);

pub(crate) struct Ink {
    pub(crate) bg: Color,
    pub(crate) raised: Color,
    pub(crate) sidebar: Color,
    pub(crate) text: Color,
    pub(crate) dim: Color,
    pub(crate) separator: Color,
    pub(crate) selected: Color,
    /// Where you are in the chrome: a row or tab, without the accent's colour.
    pub(crate) current: Color,
    pub(crate) hover: Color,
    pub(crate) accent: Color,
    /// The window's machine colour: what marks this window, its sessions and
    /// its terminals. The accent until a window knows its machine.
    pub(crate) host: Color,
    /// Text and marks on an accent fill.
    pub(crate) on_accent: Color,
    pub(crate) danger: Color,
    /// The rule between the window's regions.
    pub(crate) edge: Color,
    pub(crate) marks: Marks,
    /// The terminal's selection.
    pub(crate) selection: Selection,
    /// The shadow a floating card casts on the window.
    pub(crate) shade: Color,
    /// The tokens these fields were filled from: what a converted view
    /// reads instead (`Ink` is deleted once every view has converted).
    pub(crate) tokens: Rc<Tokens>,
}

/// The ink for a theme, or the system appearance with none: the lane-period
/// shim over [`Tokens`]. Equal arguments share one cached `Tokens`.
fn ink(dark: bool, accent: Color, theme: Option<&Theme>) -> Ink {
    Tokens::resolve(&tokens::Inputs {
        theme: theme.copied(),
        ..tokens::Inputs::system(dark, accent)
    })
    .ink()
}

thread_local! {
    /// Kit overlays (menus, selects, popovers, toast regions) a view built
    /// this run, mounted above the window by [`rune_kit::root`].
    static KIT_LAYER: RefCell<Vec<El>> = const { RefCell::new(Vec::new()) };
}

#[allow(dead_code, reason = "the kit overlays' lanes call it")]
/// Mount `overlay` in the kit layer above the window (§4.1.4). Call it
/// from a view that runs every frame, never from inside a `memo`, whose
/// closure does not run when its version holds.
pub(crate) fn kit_overlay(overlay: El) {
    KIT_LAYER.with_borrow_mut(|layer| layer.push(overlay));
}

/// The window and phone roots: phux's own surfaces in `base`, the kit's
/// overlays above them, the root carrying the kit's hover curves.
pub fn view(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    KIT_LAYER.with_borrow_mut(Vec::clear);
    let base = window(desk, cx);
    rune_kit::root(base, KIT_LAYER.with_borrow_mut(std::mem::take))
}

fn window(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let dark = crate::theme::system_dark(cx.system_dark());
    desk.follow_appearance(dark);
    if desk.phone.is_some() {
        return phone::view(desk, cx);
    }
    let p = Tokens::for_window(desk, cx).ink();
    // Native fields inherit a readable caret and selection in light themes too.
    cx.foreground(p.text);
    cx.help_look(tooltip(&p, dark));
    // `titlebar_inset` is the traffic lights' width; zero when the system
    // draws its own titlebar. On Linux, where Rune draws the window's own
    // frame (GNOME), its caption buttons take the theme's ink and their room.
    cx.caption_color(p.text);
    let insets = cx.chrome_insets();
    let lights = cx.titlebar_inset().max(insets.leading);
    let band = if lights > 0.0 { TITLEBAR_H } else { 12.0 };
    if desk.bare.is_some() || desk.quick.is_some() {
        return bare(desk, &p, lights.max(insets.height), band, cx);
    }
    let main = row()
        .key("main")
        .w_full()
        .h_full()
        .child(
            el().key("sidebar-host")
                .w(if desk.sidebar_shown() { SIDEBAR_W } else { 0.0 })
                .h_full()
                .clip_rounded(0.0)
                .motion(tok::motion::SNAPPY)
                .child(sidebar::view(desk, &p, TITLEBAR_H, lights)),
        )
        .child(
            col()
                .grow(1.0)
                .h_full()
                .min_w(0.0)
                .min_h(0.0)
                .children(
                    (!desk.settings.hover_toolbar)
                        .then(|| status::title_bar(desk, &p, lights, insets.trailing)),
                )
                .child(
                    col()
                        .grow(1.0)
                        .w_full()
                        .min_w(0.0)
                        .min_h(0.0)
                        .pl(if desk.sidebar_shown() { 0.0 } else { CARD_GAP })
                        .pr(CARD_GAP)
                        .pb(CARD_GAP)
                        .child(
                            // What floats here never takes rows from a terminal.
                            col()
                                .key("stage")
                                .grow(1.0)
                                .w_full()
                                .min_w(0.0)
                                .min_h(0.0)
                                .child(onboarding::stage(desk, &p, cx))
                                .children(status::confirm_bar(desk, &p))
                                .children(overlay::toast(desk, &p)),
                        ),
                )
                .children(status::hover_toolbar(desk, &p, lights, insets.trailing)),
        );
    // No root paste hook: it would read the clipboard before anything here
    // saw the key. ⌘V and Edit ▸ Paste come to `Desk::key` instead, which
    // answers to the modal and keeps the terminal asked for.
    let root = chrome_ground(el().w_full().h_full(), desk.glass(), p.sidebar)
        .on_layout(|d: &mut Desk, rect| d.window_laid_out(rect[2]))
        .child(main);
    floating(root, desk, &p, band, cx)
}

/// How much of the theme tint full glass still keeps, so text stays readable.
const GLASS_TINT_FLOOR: f32 = 0.18;

/// The window's ground: solid `tint`, or at `glass` above 0 the system's
/// Liquid Glass under that tint, thinned as glass rises. The material replaces
/// a node's own fill, so the tint is its first child.
#[allow(clippy::disallowed_methods)] // converted in the values commit
fn chrome_ground(root: El, glass: f32, tint: Color) -> El {
    if glass <= 0.0 {
        return root.fill(tint);
    }
    let alpha = 1.0 - glass * (1.0 - GLASS_TINT_FLOOR);
    root.system_material(rune::SystemMaterial::Glass)
        .child(el().cover().fill(tint.alpha(alpha)).inert())
}

/// A tooltip in the chrome's own colours, its chord in keycaps, on every
/// platform: quick to follow the pointer along a row of buttons.
#[allow(clippy::disallowed_methods)] // converted in the values commit
fn tooltip(p: &Ink, dark: bool) -> rune::HelpLook {
    rune::HelpLook::system(dark)
        .fill(p.raised)
        .ink(p.text)
        .ring(1.0, p.text.alpha(0.14))
        .radius(7.0)
        .padding(8.0, 5.0)
        .font(TextStyle::new(11.5, 500.0).lh(16.0))
        .keycaps(
            p.text.alpha(0.09),
            p.dim,
            TextStyle::new(10.5, 600.0).lh(11.0),
            false,
        )
        .skip_delay(0.4)
        .fade(0.12, 0.08, 0.96)
}

/// What floats over the window: banners, cards, sheets and menus.
fn floating(root: El, desk: &Desk, p: &Ink, band: f32, cx: &mut Cx<Desk>) -> El {
    let root = root
        .children(status::banner(desk, p))
        .children(updates::card(desk, p, band));
    floating_surfaces(root, desk, p, band, cx)
}

/// Paint exactly the ordering that determines keyboard, IME and paste
/// ownership. The phone places its keyboard after these layers.
fn floating_surfaces(mut root: El, desk: &Desk, p: &Ink, top: f32, cx: &mut Cx<Desk>) -> El {
    for surface in desk.surfaces() {
        root = root.children(surface_view(surface, desk, p, top, cx));
    }
    root
}

fn surface_view(surface: Surface, desk: &Desk, p: &Ink, top: f32, cx: &mut Cx<Desk>) -> Option<El> {
    match surface {
        Surface::PhoneNavigation => phone::navigation::view(desk, p, top),
        Surface::Inbox => Some(inbox::view(desk, p)),
        Surface::Playground(_) => playground::open(desk, p, cx),
        // A phone's Settings is already painted in its stage, not a scrim.
        Surface::Settings => desk.phone.is_none().then(|| settings::view(desk, p)),
        Surface::Machines => machines::sheet(desk, p),
        Surface::News => Some(news::view(desk, p)),
        Surface::Changes => Some(changes::view(desk, p)),
        Surface::Popup => popup::view(desk, p),
        Surface::Rename => tabs::rename_sheet(desk, p),
        Surface::Palette => Some(palette::view(
            desk,
            p,
            if desk.phone.is_some() { 8.0 } else { 96.0 },
        )),
        Surface::ContextMenu => desk
            .pointer
            .menu
            .map(|at| palette::context_menu(desk, at, p)),
        // Kit overlays mount in the kit layer (`kit_overlay`), not here.
        Surface::Menu(_) | Surface::Select(_) | Surface::Toasts => None,
    }
}

/// The bare window: the pane edge to edge under a band the traffic lights
/// and a drag can use, with the same sheets over it.
fn bare(desk: &Desk, p: &Ink, lights: f32, band_h: f32, cx: &mut Cx<Desk>) -> El {
    let band = el()
        .key("titlebar")
        .w_full()
        .h(if lights > 0.0 { TITLEBAR_H } else { 0.0 })
        .window_drag();
    let stage = col()
        .key("stage")
        .grow(1.0)
        .w_full()
        .min_w(0.0)
        .min_h(0.0)
        .child(panes::view(desk, p, cx))
        .children(status::confirm_bar(desk, p))
        .children(overlay::toast(desk, p));
    let main = col().key("main").w_full().h_full().child(band).child(stage);
    let root = el().w_full().h_full().fill(p.bg).child(main);
    floating(root, desk, p, band_h, cx)
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
pub use test_support::{element, offscreen, offscreen_as_app};

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests;
