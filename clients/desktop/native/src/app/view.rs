//! Terminal-first chrome: titlebar, navigator, split tree, status, overlays.
//!
//! The window is one sheet of glass over the desktop blur. Titlebar,
//! navigator and status sit on it unframed; panes float on it as rounded
//! cards with no header. A pane's controls appear when the pointer reaches
//! its top edge. Every extent, radius and paint comes from the desk's skin
//! each frame, so a skin shift (such as into the plain terminal) morphs
//! instead of popping. The titlebar and navigator are cached views: a
//! terminal frame marks the desk dirty as their ancestor, and they stay as
//! painted until the desk notifies.
//!
//! | Module | Owns |
//! |---|---|
//! | [`titlebar`] | traffic-light lead, the main menu trigger, top tabs, actions, bell |
//! | [`navigator`] | the one sidebar: sessions, their tabs, background terminals |
//! | [`changes`] | the Changes panel: the focused pane's Git checkout |
//! | [`tabs`] | one tab renderer for the titlebar and the navigator |
//! | [`panes`] | the split tree, pane cards, their controls and the empty state |
//! | [`status`] | the exceptions-only status line and the notification peek |
//!
//! Shared pieces come from [`super::ui`]; menus from [`super::menus`].

mod changes;
mod navigator;
mod panes;
mod status;
mod tabs;
mod titlebar;

#[cfg(test)]
mod changes_tests;
#[cfg(test)]
mod tab_tests;

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, BoxShadow, Context, CursorStyle, DispatchPhase, Entity, IntoElement,
    MouseMoveEvent, MouseUpEvent, ParentElement, StyleRefinement, Styled, WeakEntity, Window,
    canvas, div, linear_color_stop, linear_gradient, px,
};
use phux_protocol::ids::ResourceKey;

use crate::settings::{Skin, TabPosition, TabsWhen};
use crate::workspace::layout;

use super::actions::{OpenPathDirectory, Run};
use super::color::from_hex;
use super::desk::{self, Desk};
use super::overlays;
use super::skin::Look;
use super::ui::shadow;

pub(super) use status::{notification_edge, notification_text};

/// Whether the skin shows workspace tabs when there are `count` of them.
fn tabs_shown(skin: &Skin, count: usize) -> bool {
    skin.chrome.tabs == TabsWhen::Always || count >= 2
}

/// Whether the desk's workspace tabs show at `position` in `skin`.
fn tabs_at(desk: &Desk, skin: &Skin, position: TabPosition) -> bool {
    let count = desk
        .link
        .as_ref()
        .map_or(0, |link| link.workspace.tabs.len());
    desk.tab_position() == position && tabs_shown(skin, count)
}

/// How see-through the skin's pane cards are, 0 (solid) to 1. Gloss
/// highlights scale with it so they fade out as the glass goes opaque.
fn gloss(skin: &Skin) -> f32 {
    (1. - skin.material.card_solid).clamp(0., 1.)
}

/// Small status chips.
fn chip_radius(skin: &Skin) -> f32 {
    skin.shape.kit_radius / 2.
}

/// The selected tab's lift, plus a specular top edge and a faint accent glow
/// while the skin is glassy.
fn tab_glow(look: &Look) -> Vec<BoxShadow> {
    let mut shadows = look.tab_shadow.clone();
    let gloss = gloss(&look.skin);
    if gloss > 0. && look.lit.a > 0. {
        let accent = from_hex(&look.chrome.theme.accent);
        shadows.push(shadow(look.lit.opacity(0.5 * gloss), 1., 0., 0., true));
        shadows.push(shadow(accent.opacity(0.1 * gloss), 0., 12., -4., false));
    }
    shadows
}

/// Terminals dragged from the navigator onto a session.
#[derive(Clone)]
struct ResourceDrag {
    keys: Vec<ResourceKey>,
    label: String,
}

impl gpui::Render for ResourceDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(12.))
            .py(px(8.))
            .rounded(px(6.))
            .bg(gpui::rgb(0x202730))
            .text_color(gpui::white())
            .text_sm()
            .child(self.label.clone())
    }
}

/// What a navigator drag carries: the row's own name, or how many terminals.
fn drag_label(keys: &[ResourceKey], name: &str) -> String {
    match keys.len() {
        0 | 1 => name.to_owned(),
        count => format!("{count} terminals"),
    }
}

/// Chrome slots refresh on desk notifications, not on terminal-only frames.
#[derive(Default)]
pub(super) struct ChromeCache {
    titlebar: Option<Entity<CachedChrome>>,
    sidebar: Option<Entity<CachedChrome>>,
    changes: Option<Entity<CachedChrome>>,
    overlays: Option<Entity<CachedChrome>>,
}

struct CachedChrome {
    desk: WeakEntity<Desk>,
    build: fn(&mut Desk, &mut Window, &mut Context<Desk>) -> AnyElement,
}

impl gpui::Render for CachedChrome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(desk) = self.desk.upgrade() else {
            return div().into_any_element();
        };
        desk.update(cx, |desk, cx| (self.build)(desk, window, cx))
    }
}

fn chrome_slot(
    slot: &mut Option<Entity<CachedChrome>>,
    build: fn(&mut Desk, &mut Window, &mut Context<Desk>) -> AnyElement,
    style: StyleRefinement,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    slot.get_or_insert_with(|| {
        let observed = cx.entity();
        cx.new(|cx| {
            cx.observe(&observed, |_, _, cx| cx.notify()).detach();
            CachedChrome {
                desk: observed.downgrade(),
                build,
            }
        })
    })
    .clone()
    .cached(style)
}

fn build_overlays(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    overlays::layer(desk, window, cx)
}

pub fn render(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    let _frame = crate::diag::FRAME.timer();
    desk.set_viewport(window.viewport_size());
    desk.sync_title(window);
    desk.settle_focus(window, cx);
    desk.drive_skin(window, cx);
    desk.ensure_clock(cx);
    desk.flush_system(cx);
    // Read after the skin is driven, so a shift paints its current step.
    let look = desk.look();
    let skin = look.skin;
    if crate::diag::evidence_pending(cx) {
        let desk_handle = cx.weak_entity();
        crate::diag::arm_evidence(
            Box::new(move |app| {
                desk_handle
                    .upgrade()
                    .map(|desk| desk.read(app).frame_report(app))
            }),
            window,
            cx,
        );
    }
    let cursor = desk.drag_cursor();
    if !cx.has_active_drag() {
        desk.drop_target = None;
    }
    observe_motion(desk, window);
    // The sidebar slides shut as the skin withdraws it, whatever the prefs say.
    let presence = skin.chrome.sidebar.clamp(0., 1.);
    let sidebar = navigator::wanted(desk) && presence > 0.;
    let sidebar_width = desk.prefs.sidebar_width as f32 * presence;
    let sheen_height = skin.material.sheen_height;
    let sheen = sheen_height > 0.
        && look.sheen.a > 0.
        && (skin.chrome.sheen || desk.shift_progress().is_some());

    div()
        .id("desk")
        .key_context("Desk")
        .track_focus(&desk.focus)
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(look.sheet)
        .when(sheen, |sheet| {
            sheet.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(sheen_height))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(look.sheen, 0.),
                        linear_color_stop(look.sheen.opacity(0.), 1.),
                    )),
            )
        })
        .text_color(look.ink)
        .on_action(cx.listener(|desk, action: &Run, window, cx| {
            desk.execute(&action.id, window, cx);
        }))
        .on_action(cx.listener(|desk, _: &OpenPathDirectory, window, cx| {
            desk.open_selected_directory(window, cx);
        }))
        .on_key_down(cx.listener(|desk, event: &gpui::KeyDownEvent, window, cx| {
            desk.on_key(event, window, cx);
        }))
        .child(chrome_slot(
            &mut desk.chrome.titlebar,
            titlebar::build,
            StyleRefinement::default()
                .w_full()
                .h(px(skin.chrome.titlebar)),
            cx,
        ))
        .child(
            div()
                .flex_grow(1.)
                .flex()
                .min_h(px(0.))
                .when(sidebar, |row| {
                    let style = StyleRefinement::default().w(px(sidebar_width)).h_full();
                    let style = if presence < 1. {
                        style.overflow_hidden().opacity(presence)
                    } else {
                        style
                    };
                    row.child(chrome_slot(
                        &mut desk.chrome.sidebar,
                        navigator::build,
                        style,
                        cx,
                    ))
                })
                .child(panes::main_column(desk, &look, window, cx))
                .when(desk.changes.open && !quick(desk), |row| {
                    row.child(chrome_slot(
                        &mut desk.chrome.changes,
                        changes::build,
                        StyleRefinement::default()
                            .w(px(changes::WIDTH))
                            .flex_shrink_0()
                            .h_full(),
                        cx,
                    ))
                }),
        )
        .child(chrome_slot(
            &mut desk.chrome.overlays,
            build_overlays,
            StyleRefinement::default().absolute().inset_0().size_full(),
            cx,
        ))
        .children(cursor.map(|cursor| drag_capture(cursor, cx)))
}

/// Feed this frame's placements to the motion clock, and keep frames coming
/// while a pane is still growing in or folding away.
fn observe_motion(desk: &mut Desk, window: &mut Window) {
    let now = Instant::now();
    let tab = desk
        .link
        .as_ref()
        .and_then(|link| link.workspace.active_tab());
    let placements = tab
        .map(|tab| {
            layout::placements(&tab.root)
                .into_iter()
                .map(|placement| placement.id.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    desk.motion
        .observe(tab.map(|tab| tab.id.as_str()), &placements, now);
    if desk.motion.active(now) {
        window.request_animation_frame();
    }
}

/// While a divider, the sidebar edge or a tab is dragged, follow the pointer
/// anywhere in the window, not only over the element that started the drag.
fn drag_capture(cursor: CursorStyle, cx: &mut Context<Desk>) -> AnyElement {
    let handle = cx.weak_entity();
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            window.set_window_cursor_style(cursor);
            let moves = handle.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    let (x, y) = (event.position.x.as_f32(), event.position.y.as_f32());
                    let pressed = event.pressed_button.is_some();
                    let _ = moves.update(cx, |desk, cx| desk.drag_move(x, y, pressed, cx));
                }
            });
            let ups = handle.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    let _ = ups.update(cx, |desk, cx| desk.end_drag(cx));
                }
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// Whether this is the quick-terminal window.
fn quick(desk: &Desk) -> bool {
    desk.role == desk::WindowRole::Quick
}
