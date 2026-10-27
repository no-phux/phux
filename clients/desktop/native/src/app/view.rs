//! Terminal-first chrome: titlebar, navigator, split tree, overlays.
//!
//! The window is one sheet of glass over the desktop blur. The titlebar
//! and navigator sit on it unframed; panes float on it as rounded
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
//! | [`tabs`] | one tab renderer for the titlebar, bottom strip and navigator |
//! | [`panes`] | the split tree, pane cards, their controls and the empty state |
//! | [`status`] | exception chips, the Follow live pill and the notification peek |
//!
//! Shared pieces come from [`super::ui`]; menus from [`super::menus`].

mod changes;
mod navigator;
mod panes;
mod reviews;
mod status;
mod tab_peek;
mod tabs;
mod titlebar;

#[cfg(feature = "ui-fixtures")]
pub(super) mod preview;

#[cfg(test)]
mod changes_tests;
#[cfg(test)]
mod reviews_tests;
#[cfg(test)]
mod tab_tests;

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnimationExt, AnyElement, Context, CursorStyle, DispatchPhase, Entity, IntoElement,
    MouseMoveEvent, MouseUpEvent, ParentElement, StyleRefinement, Styled, WeakEntity, Window,
    canvas, div, linear_color_stop, linear_gradient, px, relative,
};
use phux_protocol::ids::ResourceKey;

use crate::settings::{Skin, TabPosition, TabsWhen};
use crate::workspace::layout;

use super::actions::{OpenPathDirectory, Run};
use super::desk::{self, Desk};
use super::overlays;
use super::skin::Look;

pub(super) use changes::width as changes_width;
pub(super) use navigator::{foldable_sessions, listed_sessions};
pub(super) use navigator::group as group_work;
pub(super) use status::{notification_edge, notification_text};
pub(super) use tabs::MIN_STRIP_WIDTH;

/// The spring the chrome's edges ride when the sidebar or the Changes panel
/// opens or closes: critically damped, so everything that follows an edge
/// lands with it, together, without overshoot.
pub(super) const CHROME_SPRING: gpui::SpringConfig = gpui::SpringConfig::new(600., 49., 1.);

/// Whether the chrome moves: the Animations preference and the system's
/// reduced motion both stop it.
pub(super) fn chrome_moves(desk: &Desk, cx: &gpui::App) -> bool {
    desk.prefs.animations && !cx.reduce_motion()
}

/// [`CHROME_SPRING`] to `target`, snapping there when the chrome does not move.
pub(super) fn chrome_spring(moves: bool, target: f32) -> gpui::SpringAnimation<f32> {
    let spring = gpui::SpringAnimation::new(CHROME_SPRING).to(target);
    if moves {
        spring
    } else {
        spring.playback(gpui::SpringPlayback::Completed)
    }
}

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

/// Off-top tabs let the sidebar own the window header. The workspace then
/// reaches the top of the sheet instead of preserving an empty tab strip.
fn integrated_titlebar(desk: &Desk, skin: &Skin) -> bool {
    desk.sidebar_shown() && skin.chrome.sidebar > 0. && desk.tab_position() != TabPosition::Top
}

fn titlebar_height(desk: &Desk, skin: &Skin) -> f32 {
    if !quick(desk) && desk.prefs.tab_position != TabPosition::Top {
        skin.chrome.titlebar.min(38.)
    } else {
        skin.chrome.titlebar
    }
}

/// The workspace's offset from the window top, also used by floating cards.
pub(crate) fn content_top(desk: &Desk, skin: &Skin) -> f32 {
    if integrated_titlebar(desk, skin) {
        0.
    } else {
        titlebar_height(desk, skin)
    }
}

/// How see-through the skin's pane cards are, 0 (solid) to 1. Gloss
/// highlights scale with it so they fade out as the glass goes opaque.
fn gloss(skin: &Skin) -> f32 {
    (1. - skin.material.card_solid).clamp(0., 1.)
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
    bottom_tabs: Option<Entity<CachedChrome>>,
    tab_rail: Option<Entity<CachedChrome>>,
    sidebar: Option<Entity<CachedChrome>>,
    changes: Option<Entity<CachedChrome>>,
    reviews: Option<Entity<CachedChrome>>,
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

/// A cached chrome region laid out at `style`. A cached view is a leaf to
/// layout, with no measured content to defend its size, so the slot never
/// shrinks: whatever the active tab holds, the chrome keeps the size its
/// style names, and only an intended change (a drag, a pref, the skin) moves it.
fn chrome_slot(
    slot: &mut Option<Entity<CachedChrome>>,
    build: fn(&mut Desk, &mut Window, &mut Context<Desk>) -> AnyElement,
    style: StyleRefinement,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let style = style.flex_shrink_0();
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
    let look = desk.look();
    div()
        .size_full()
        .relative()
        .children(super::updates::card(desk, &look, window, cx))
        .children(super::desk::cli_path::card(desk, &look, window, cx))
        .child(overlays::layer(desk, window, cx))
        .into_any_element()
}

pub fn render(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> impl IntoElement {
    let _frame = crate::devtools::frames::FrameTimer::start();
    let tab_position = desk.tab_position();
    desk.set_viewport(window.viewport_size());
    desk.settle_tab_peek();
    desk.settle_leader(window, cx);
    if desk.tab_position() != tab_position {
        // Cached chrome must follow a responsive placement change on resize.
        cx.notify();
    }
    desk.sync_title(window);
    desk.settle_focus(window, cx);
    desk.settle_reviews_focus(window, cx);
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
    let sheen_height = skin.material.sheen_height;
    let sheen = sheen_height > 0.
        && look.sheen.a > 0.
        && (skin.chrome.sheen || desk.shift_progress().is_some());
    let backdrop_shader = desk.backdrop_layer(cx);

    div()
        .id("desk")
        .key_context("Desk")
        .track_focus(&desk.focus)
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(look.sheet)
        .children(backdrop_shader)
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
        .line_height(relative(super::ui::typography::LEADING))
        .on_action(cx.listener(|desk, action: &Run, window, cx| {
            desk.execute(&action.id, window, cx);
        }))
        .capture_key_up(cx.listener(|desk, event: &gpui::KeyUpEvent, _, _| {
            desk.release_leader(&event.keystroke);
        }))
        .capture_any_mouse_down(cx.listener(|desk, _, _, cx| {
            desk.cancel_leader(cx);
        }))
        .on_action(cx.listener(|desk, _: &OpenPathDirectory, window, cx| {
            desk.open_selected_directory(window, cx);
        }))
        .on_key_down(cx.listener(|desk, event: &gpui::KeyDownEvent, window, cx| {
            desk.on_key(event, window, cx);
        }))
        .when(!integrated_titlebar(desk, &skin), |sheet| {
            let height = titlebar_height(desk, &skin);
            sheet.child(chrome_slot(
                &mut desk.chrome.titlebar,
                titlebar::build,
                StyleRefinement::default().w_full().h(px(height)),
                cx,
            ))
        })
        .child(workspace_row(desk, &look, window, cx))
        .child(chrome_slot(
            &mut desk.chrome.overlays,
            build_overlays,
            StyleRefinement::default().absolute().inset_0().size_full(),
            cx,
        ))
        .children(super::hud::element(desk.prefs.font_family.clone().into()))
        .children(desk.leader_hint())
        .children(cursor.map(|cursor| drag_capture(cursor, cx)))
}

fn workspace_row(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let skin = &look.skin;
    if desk.reviews.open && !quick(desk) {
        return div().flex_1().flex().min_h_0().child(chrome_slot(
            &mut desk.chrome.reviews,
            reviews::build,
            StyleRefinement::default().flex_1().h_full(),
            cx,
        ));
    }
    let presence = skin.chrome.sidebar.clamp(0., 1.);
    let wanted = navigator::wanted(desk);
    let sidebar =
        presence > 0. && !quick(desk) && (wanted || desk.disclosing(desk::SIDEBAR_DISCLOSURE));
    let sidebar_width = desk.prefs.sidebar_width as f32 * presence;
    let sidebar_reveal = reveal(
        "sidebar-reveal",
        sidebar_width,
        desk.disclosing(desk::SIDEBAR_DISCLOSURE).then_some(wanted),
    );
    div()
        .flex_1()
        .flex()
        .min_h_0()
        .when(sidebar, |row| {
            let style = StyleRefinement::default().w(px(sidebar_width)).h_full();
            let style = if presence < 1. {
                style.overflow_hidden().opacity(presence)
            } else {
                style
            };
            let slot = chrome_slot(&mut desk.chrome.sidebar, navigator::build, style, cx);
            row.child(sidebar_reveal(div().justify_end().child(slot)))
        })
        .when(
            desk.tab_rail() && tabs_at(desk, skin, TabPosition::Side),
            |row| {
                row.child(chrome_slot(
                    &mut desk.chrome.tab_rail,
                    tabs::build_rail,
                    StyleRefinement::default().w(px(tabs::RAIL_WIDTH)).h_full(),
                    cx,
                ))
            },
        )
        .child(panes::main_column(desk, look, window, cx))
        .children(changes_slot(desk, window, cx))
}

fn changes_slot(desk: &mut Desk, window: &Window, cx: &mut Context<Desk>) -> Option<AnyElement> {
    if quick(desk) || !(desk.changes.open || desk.disclosing(desk::CHANGES_DISCLOSURE)) {
        return None;
    }
    let width = changes::width(desk, window.viewport_size().width.as_f32());
    let reveal = reveal(
        "changes-reveal",
        width,
        desk.disclosing(desk::CHANGES_DISCLOSURE)
            .then_some(desk.changes.open),
    );
    let slot = chrome_slot(
        &mut desk.chrome.changes,
        changes::build,
        StyleRefinement::default().w(px(width)).h_full(),
        cx,
    );
    Some(reveal(div().child(slot)))
}

/// A side panel's slot, `width` wide at rest. While the panel opens or
/// closes (`opening` is `Some`) the slot rides [`CHROME_SPRING`] between shut
/// and `width`; the panel keeps its own width and overflows past the window
/// edge it sits against, so it slides rather than squeezes. At rest the
/// spring snaps, so a resize drag tracks the pointer exactly. The slot keeps
/// one element id throughout, so the panel's scroll state survives.
fn reveal(
    id: &'static str,
    width: f32,
    opening: Option<bool>,
) -> impl FnOnce(gpui::Div) -> AnyElement {
    move |slot| {
        let spring = match opening {
            Some(true) => chrome_spring(true, width).from(0.),
            Some(false) => chrome_spring(true, 0.),
            None => chrome_spring(false, width),
        };
        slot.h_full()
            .flex_shrink_0()
            .flex()
            .with_spring(id, spring, |slot, width| slot.w(px(width.max(0.))))
            .into_any_element()
    }
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
