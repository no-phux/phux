//! The window: a session sidebar, the terminal, and a status line.

use std::cell::RefCell;
#[cfg(test)]
use std::rc::Rc;

use rune::{Cx, El, col, el, row};

use crate::desk::Desk;
use crate::desk::surfaces::Surface;
use crate::features::changes;
use phux_config_core::rune::TabBar;

mod badge;
mod chief;
mod controls;
pub(crate) use controls::{
    Cover, Sheet, button, icon_press, keep_from_pane, keycap, lifted, pressable, reveal, scrim,
    sheet_header, tip,
};
mod documents;
#[allow(clippy::disallowed_methods)] // illustration art
mod galaxy;
#[allow(clippy::disallowed_methods)] // illustration art
mod hive;
mod icons;
mod inbox;
#[allow(clippy::disallowed_methods)] // illustration art
mod kitchen;
mod machines;
mod news;
mod onboarding;
mod overlay;
pub(crate) use overlay::key_hint;
mod pair;
mod palette;
mod pane_marks;
mod panes;
mod phone;
pub(crate) use phone::edge_color as phone_edge_color;
#[allow(clippy::disallowed_methods)] // illustration art
mod pitch;
#[allow(clippy::disallowed_methods)] // illustration art
mod pitlane;
mod playground;
mod popup;
mod questions;
#[allow(clippy::disallowed_methods)] // illustration art
mod reef;
pub(crate) mod rows;
mod settings;
mod sidebar;
pub(crate) use sidebar::focus_target as sidebar_focus_target;
mod spatial;
mod status;
mod tabs;
mod tile;
#[allow(clippy::disallowed_methods)] // owner path: the tokens' own raw values
pub(crate) mod tokens;
mod updates;

pub(crate) use tokens::{Paint, Tokens};

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
/// Space between the terminal area's edge and its grid.
pub(crate) use tokens::TERMINAL_PAD as PAD;

/// The tokens for a theme, or the system appearance with none, as a
/// window with no machine, glass or accessibility setting has them.
#[cfg(test)]
fn tokens_for(dark: bool, theme: Option<&crate::theme::Theme>) -> Rc<Tokens> {
    Tokens::resolve(&tokens::Inputs {
        theme: theme.copied(),
        ..tokens::Inputs::system(dark)
    })
}

thread_local! {
    /// Kit overlays (menus, selects, popovers, toast regions) a view built
    /// this run, mounted above the window by [`rune_kit::root`].
    static KIT_LAYER: RefCell<Vec<El>> = const { RefCell::new(Vec::new()) };
}

/// Mount `overlay` in the kit layer above the window. Call it
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

/// What every run of the view sets on `cx`, the phone's included.
fn wire(t: &Tokens, cx: &mut Cx<Desk>) {
    let text = t.roles.text.text.paint();
    // Native fields inherit a readable caret and selection in light themes too.
    cx.foreground(text);
    // On Linux, where Rune draws the window's own frame (GNOME), its caption
    // buttons take the theme's ink.
    cx.caption_color(text);
    let ring = t.space.focus_ring;
    cx.focus_ring(ring.width, ring.offset, t.roles.border.focus.paint());
    cx.help_look(tooltip(t));
    cx.titlebar_height(t.space.titlebar_h);
    // `cx.web_focus()` (every pressable a Tab stop) stays off: it
    // moves a modal's first focus and focuses controls on press, which
    // changes Settings search and the context menu's keyboard walk
    // (DEVELOP.md, "Kit deviations"). Rows and kit controls are focusable.
}

fn window(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let dark = crate::theme::system_dark(cx.system_dark());
    desk.follow_appearance(dark);
    let t = Tokens::for_window(desk, cx);
    wire(&t, cx);
    if desk.phone.is_some() {
        return phone::view(desk, cx);
    }
    cx.zoom(desk.ui_zoom());
    let p = &*t;
    let s = &t.space;
    // `titlebar_inset` is the traffic lights' width; zero when the system
    // draws its own titlebar.
    let insets = cx.chrome_insets();
    let lights = cx.titlebar_inset().max(insets.leading);
    let band = if lights > 0.0 { s.titlebar_h } else { s.s3 };
    if desk.bare.is_some() || desk.quick.is_some() {
        return bare(desk, p, lights.max(insets.height), band, cx);
    }
    let flush = desk.flush();
    let main = row()
        .key("main")
        .w_full()
        .h_full()
        .child(
            el().key("sidebar-host")
                .w(if desk.sidebar_shown() { s.sidebar_w } else { 0.0 })
                .h_full()
                .clip_rounded(0.0)
                // The drawer spring; Rune holds it still under reduced motion.
                .motion(t.motion.drawer.enter)
                .child(sidebar::view(desk, p, s.titlebar_h, lights)),
        )
        .child(
            col()
                .grow(1.0)
                .h_full()
                .min_w(0.0)
                .min_h(0.0)
                // Flush, the band and the pane are one surface: the
                // terminal's ground under both, the card's gutters, rim
                // and lift gone (`Desk::flush`).
                .fill(if flush {
                    t.roles.layer.ground.paint()
                } else {
                    rune::Color::TRANSPARENT
                })
                // On the drawer's spring, as the sidebar is: a fold moves
                // the drawer, the gutters and the panes as one, so the
                // pane's far edge never runs past the window or short of it.
                .token_motion(t.motion.drawer.enter)
                .children(status::top(desk, p, lights, insets.trailing))
                .child(
                    col()
                        .grow(1.0)
                        .w_full()
                        .min_w(0.0)
                        .min_h(0.0)
                        .pl(if desk.sidebar_shown() || flush {
                            0.0
                        } else {
                            s.card_gap
                        })
                        .pr(if flush { 0.0 } else { s.card_gap })
                        .pb(if flush { 0.0 } else { s.card_gap })
                        .child(
                            // What floats here never takes rows from a terminal.
                            col()
                                .key("stage")
                                .grow(1.0)
                                .w_full()
                                .min_w(0.0)
                                .min_h(0.0)
                                .child(onboarding::stage(desk, p, cx))
                                .children(status::confirm_bar(desk, p))
                                .children(overlay::toast(desk, p)),
                        ),
                )
                .children(status::tab_row(desk, p, TabBar::Bottom))
                .children(status::hover_toolbar(desk, p, lights, insets.trailing)),
        );
    // No root paste hook: it would read the clipboard before anything here
    // saw the key. ⌘V and Edit ▸ Paste come to `Desk::key` instead, which
    // answers to the modal and keeps the terminal asked for.
    let root = ground(el().w_full().h_full(), desk.glass(), &t)
        .on_layout(|d: &mut Desk, rect| d.window_laid_out([rect[2], rect[3]]))
        .child(main);
    floating(root, desk, p, band, cx)
}

/// The window's ground: the sidebar layer, or at `glass` above 0 the
/// system's Liquid Glass under that layer as a tint, thinned as glass rises
/// down to the verified floor. The material replaces a node's own
/// fill, so the tint is its first child. Increased contrast, reduce
/// transparency and forced colours draw it opaque.
fn ground(root: El, glass: f32, t: &Tokens) -> El {
    glassed(root, t.roles.layer.sidebar.paint(), glass, t)
}

/// `root` filled with `tint`, or the system's glass under `tint` at the
/// alpha the `glass` fraction gives (`Tokens::glass_tint_alpha`), composed
/// blend-correct (`Tokens::alpha`).
fn glassed(root: El, tint: rune::Color, glass: f32, t: &Tokens) -> El {
    let alpha = t.glass_tint_alpha(glass);
    if alpha >= 1.0 {
        return root.fill(tint);
    }
    root.system_material(rune::SystemMaterial::Glass)
        .child(el().cover().fill(t.alpha(tint, alpha)).inert())
}

/// The kit's tooltip on the overlay layer at the menu radius, its chord in
/// keycaps, on every platform.
fn tooltip(t: &Tokens) -> rune::HelpLook {
    rune_kit::TooltipStyle::from_kit(&t.kit, rune_kit::TooltipAppearance::Compact)
        .radius(t.radius.md)
        .fill(t.roles.layer.overlay.paint())
        .help_look()
}

/// What floats over the window: banners, cards, sheets and menus.
fn floating(root: El, desk: &Desk, p: &Tokens, band: f32, cx: &mut Cx<Desk>) -> El {
    let root = root
        .children(status::banner(desk, p))
        .children(updates::card(desk, p, band));
    floating_surfaces(root, desk, p, band, cx)
}

/// Paint exactly the ordering that determines keyboard, IME and paste
/// ownership. The phone places its keyboard after these layers.
fn floating_surfaces(mut root: El, desk: &Desk, p: &Tokens, top: f32, cx: &mut Cx<Desk>) -> El {
    for surface in desk.surfaces() {
        root = root.children(surface_view(surface, desk, p, top, cx));
    }
    root
}

fn surface_view(
    surface: Surface,
    desk: &Desk,
    p: &Tokens,
    top: f32,
    cx: &mut Cx<Desk>,
) -> Option<El> {
    match surface {
        Surface::PhoneNavigation => phone::navigation::view(desk, top),
        Surface::Inbox => Some(inbox::view(desk, p)),
        Surface::Playground(_) => playground::open(desk, p, cx),
        // A phone's Settings is already painted in its stage, not a scrim.
        Surface::Settings => desk.phone.is_none().then(|| settings::view(desk, p)),
        Surface::Machines => machines::sheet(desk, p),
        Surface::News => Some(news::view(desk, p)),
        Surface::Changes => Some(changes::view(desk, p)),
        Surface::Popup => popup::view(desk, p),
        Surface::Rename => tabs::rename_sheet(desk, p),
        Surface::Chief => Some(chief::view(desk, p, tokens::layout::OVERLAY.palette_top)),
        Surface::Palette => Some(palette::view(
            desk,
            p,
            if desk.phone.is_some() {
                tokens::layout::OVERLAY.palette_top_phone
            } else {
                tokens::layout::OVERLAY.palette_top
            },
        )),
        // A terminal's or a tab's menu at the pointer; it mounts itself in
        // the kit layer (`kit_overlay`).
        Surface::ContextMenu => desk
            .pointer
            .menu
            .map(|at| palette::context_menu(desk, at, p)),
        // The toasts are drawn in the stage, clear of the live pill
        // (`overlay::toast`).
        Surface::Toasts => None,
    }
}

/// The bare window: the pane edge to edge under a band the traffic lights
/// and a drag can use, with the same sheets over it.
fn bare(desk: &Desk, p: &Tokens, lights: f32, band_h: f32, cx: &mut Cx<Desk>) -> El {
    let band = el()
        .key("titlebar")
        .w_full()
        .h(if lights > 0.0 {
            p.space.titlebar_h
        } else {
            0.0
        })
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
    let root = el()
        .w_full()
        .h_full()
        .fill(p.roles.layer.ground.paint())
        .child(main);
    floating(root, desk, p, band_h, cx)
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
pub use test_support::{
    element, glass_panels, offscreen, offscreen_as_app, offscreen_as_app_with_renderer,
};

#[cfg(test)]
mod tests;
