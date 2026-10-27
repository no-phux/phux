//! One pane card: its glass, its depth, and what it shows.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, ExternalPaths, FileDropEvent, Hsla, IntoElement,
    ParentElement, SharedString, Styled, Window, div, px,
};

use crate::workspace::layout::{self, Placement};

use super::super::super::color::from_hex;
use super::super::super::conversation;
use super::super::super::desk::Desk;
use super::super::super::overlays;
use super::super::super::pane_identity::{PaneKind, PaneOrigin};
use super::super::super::skin::{Look, Surface, Surfaced};
use super::super::super::ui::status::harness_mark;
use super::super::super::ui::{ghost_button, pill, specular};
use super::super::gloss;
use super::controls::{Offer, drop_hint, pane_controls};
use super::depth::{depth_inset, veils};

/// How far down a pane the pointer reveals its controls.
const CONTROLS_EDGE: f32 = 36.;

/// A terminal that has not bound a view yet. A saved id that this server
/// does not have says so. A spawn waiting for its replica stays blank.
fn pending_terminal(missing: bool, look: &Look) -> AnyElement {
    if !missing {
        return div().size_full().into_any_element();
    }
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(from_hex(&look.chrome.muted))
        .child(
            div()
                .w_full()
                .text_center()
                .child("Saved terminal is not on this server"),
        )
        .into_any_element()
}

/// A terminal pane's body: its cached view, or a placeholder while the
/// terminal is missing or not ready.
fn terminal_body(
    desk: &mut Desk,
    placement: &Placement,
    focused: bool,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let placement_id = placement.id.clone();
    // The terminal holds the keyboard only while no modal or find field does.
    let settings = desk.terminal_settings(placement, focused && !desk.text_field_has_keys());
    let client = desk.client();
    let Some(terminal) = placement.terminal_id.clone() else {
        return pending_terminal(true, look);
    };
    let Some(view) = placement.view_id else {
        return pending_terminal(false, look);
    };
    let (Some(settings), Some(client)) = (settings, client) else {
        return pending_terminal(false, look);
    };
    let remote = crate::platform::filesystem_host(desk.remote.as_deref()).map(str::to_owned);
    let entity = desk.panes_mut().entity_for(
        &placement_id,
        &terminal,
        view,
        settings,
        &client,
        window,
        cx,
    );
    entity.update(cx, |terminal, cx| {
        terminal.set_window_remote(remote, cx);
    });
    // A cached view keeps its last paint until this entity is notified.
    // Chrome refreshes (hover, clock, sidebar) must not reacquire and
    // reshape the terminal. GPUI requires a definite size for that cache.
    // A right press the terminal did not report to its program bubbles here.
    div()
        .size_full()
        .on_mouse_down(
            gpui::MouseButton::Right,
            cx.listener(move |desk, event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                desk.open_terminal_menu(&placement_id, event.position, window, cx);
            }),
        )
        .child(entity.cached(gpui::StyleRefinement::default().size_full()))
        .into_any_element()
}

/// One pane: a rounded card on the glass, with no header. Its controls float
/// in when the pointer reaches the top edge, and a file dragged over it shows
/// where the drop will paste.
pub(super) fn pane_view(
    desk: &mut Desk,
    placement: &Placement,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let presence = desk.motion.presence(&placement.id, Instant::now());
    let focused =
        desk.focused_placement().map(|item| item.id.as_str()) == Some(placement.id.as_str());
    let look = desk.look();
    let colors = &look.chrome;
    let placement_id = placement.id.clone();
    desk.prepare_pending_native(&placement_id, window, cx);
    let identity = desk.pane_identity(placement);
    // A new pane kind adds its body here; everything else styles by identity.
    let body = match identity.kind {
        PaneKind::Conversation => conversation::render(desk, &placement_id, colors, window, cx),
        PaneKind::Terminal => terminal_body(desk, placement, focused, &look, window, cx),
    };
    let tab = desk.active_tab();
    let zoomed = tab.is_some_and(|tab| tab.zoomed_id.is_some());
    let panes = tab.map_or(0, |tab| layout::placements(&tab.root).len());
    // Focus only shows among several visible panes.
    let shared = !zoomed && panes > 1;
    // While zoomed, the rest of the tab waits behind this pane.
    let behind = if zoomed { panes.saturating_sub(1) } else { 0 };
    let find = desk
        .find
        .as_ref()
        .filter(|find| find.placement_id == placement.id)
        .map(|find| find.case_sensitive);
    let find_bar = find.map(|case| overlays::find_bar(desk, case, cx));
    let id = placement.id.clone();
    let leaving = desk.motion.is_leaving(&id);
    let depth = pane_depth(desk, &id, identity.origin, focused, window, cx);
    let dim = pane_dim(desk, &look, focused, shared, depth);
    let (padding_x, padding_y) = (desk.prefs.padding_x as f32, desk.prefs.padding_y as f32);
    let (inset_x, inset_y) = depth_inset(&look, depth, padding_x, padding_y);
    let fade = from_hex(&colors.theme.background);
    let dropping = desk.drop_target.as_deref() == Some(id.as_str());
    let remote_files = placement.terminal_id.as_ref().is_some_and(|terminal| {
        !matches!(
            crate::attach::terminal_host(
                crate::platform::filesystem_host(desk.remote.as_deref()),
                terminal
            ),
            crate::attach::TerminalHost::Local
        )
    });
    // Only the focused pane is read for scrollback; the pill sits on it.
    let follow = focused && !leaving && desk.follows_live();
    let controls = controls_visible(desk, &id, find.is_some(), dropping, leaving);
    let edge = if dropping {
        from_hex(&colors.theme.accent)
    } else {
        gpui::transparent_black()
    };
    let shine = focus_shine(&look, focused, shared, depth, presence);
    let controls_bar = controls.then(|| controls_bar(desk, placement, zoomed, behind, &look, cx));
    let corner = pane_corner(desk, placement, depth, controls, &look, cx);
    let card = div()
        .id(SharedString::from(format!("pane-card-{id}")))
        .debug_selector({
            let id = id.clone();
            move || format!("pane-card-{id}")
        })
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .when(presence < 1., |card| card.opacity(presence.max(0.)))
        .surface(&look, Surface::Pane)
        .border_color(edge)
        .shadow(look.pane_depth_shadows(depth))
        .overflow_hidden()
        .block_mouse_except_scroll()
        .on_hover(track_hover(id.clone(), |desk| &mut desk.motion.hover, cx));
    let card = file_drop_target(card, &id, cx)
        .child(
            div()
                .flex_grow(1.)
                .min_h(px(0.))
                .relative()
                .px(px(padding_x - inset_x))
                .py(px(padding_y - inset_y))
                .child(body)
                .children(
                    dim.map(|amount| div().absolute().inset_0().bg(Hsla { a: amount, ..fade })),
                ),
        )
        .child(
            // Reveals the controls. It holds no other listener, so presses and
            // drags still reach the terminal under it.
            div()
                .id(SharedString::from(format!("pane-edge-{id}")))
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(CONTROLS_EDGE))
                .on_hover(track_hover(id.clone(), |desk| &mut desk.chrome_hover, cx)),
        )
        .children(corner)
        .children(controls_bar)
        .when(dropping, |card| card.child(drop_hint(&look, remote_files)))
        .when(follow, |card| {
            let moves = super::super::chrome_moves(desk, cx);
            card.child(super::super::status::follow_live(&look, moves, cx))
        })
        .children(find_bar);
    // The slot keeps the split's geometry; the card draws in from its edges.
    // The highlight lies on the card's top border row, outside the card's
    // clip, which excludes a painted border.
    div()
        .size_full()
        .relative()
        .px(px(inset_x))
        .py(px(inset_y))
        .child(card)
        .children(
            specular(shine, inset_x + look.skin.shape.radius).map(|line| line.top(px(inset_y))),
        )
        .into_any_element()
}

/// Focus among split panes is a neutral top edge, never a coloured perimeter.
fn focus_shine(look: &Look, focused: bool, shared: bool, depth: f32, presence: f32) -> Hsla {
    if !focused || !shared {
        return gpui::transparent_black();
    }
    look.lit
        .opacity(0.7 * gloss(&look.skin) * (1. - depth) * presence)
}

fn pane_dim(desk: &Desk, look: &Look, focused: bool, shared: bool, depth: f32) -> Option<f32> {
    let unfocused = if !focused && shared {
        1. - desk.prefs.unfocused_opacity as f32
    } else {
        0.
    };
    Some(veils(unfocused, look.skin.material.depth.veil * depth)).filter(|amount| *amount > 0.)
}

/// The hover capsule offers conversation setup only where it can be enabled.
fn controls_bar(
    desk: &Desk,
    placement: &Placement,
    zoomed: bool,
    behind: usize,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let origin = desk.pane_identity(placement).origin;
    let eligible = desk.conversation_eligible(&placement.id);
    let setup = origin
        .harness()
        .filter(|_| desk.prefs.native_agents && !eligible)
        .filter(|_| placement.terminal_id.is_some());
    pane_controls(
        desk,
        placement,
        origin,
        zoomed,
        Offer { behind, setup },
        look,
        cx,
    )
    .into_any_element()
}

/// File hover and drop share the same placement, without changing its layout.
fn file_drop_target(
    card: gpui::Stateful<gpui::Div>,
    id: &str,
    cx: &Context<Desk>,
) -> gpui::Stateful<gpui::Div> {
    let drag_id = id.to_owned();
    let drop_id = id.to_owned();
    let exit_id = id.to_owned();
    card.on_drag_move::<ExternalPaths>(cx.listener(
        move |desk, event: &DragMoveEvent<ExternalPaths>, _, cx| {
            let inside = event.bounds.contains(&event.event.position);
            let here = desk.drop_target.as_deref() == Some(drag_id.as_str());
            if inside && !here {
                desk.drop_target = Some(drag_id.clone());
                cx.notify();
            } else if !inside && here {
                desk.drop_target = None;
                cx.notify();
            }
        },
    ))
    .on_drop(cx.listener(move |desk, paths: &ExternalPaths, window, cx| {
        if let Some(tab) = desk.active_tab_id() {
            desk.focus_placement(&tab, &drop_id, window, cx);
        }
        desk.drop_paths(&drop_id, paths.paths(), cx);
        desk.after_command(window, cx);
    }))
    .on_file_drop_exit(cx.listener(move |desk, _: &FileDropEvent, _, cx| {
        if desk.drop_target.as_deref() == Some(exit_id.as_str()) {
            desk.drop_target = None;
            cx.notify();
        }
    }))
}

/// An agent's pane sits back until it holds focus or the pointer.
fn pane_depth(
    desk: &mut Desk,
    id: &str,
    origin: PaneOrigin,
    focused: bool,
    window: &mut Window,
    cx: &Context<Desk>,
) -> f32 {
    let hovered = desk.motion.hover.as_deref() == Some(id);
    let recessed = origin.is_agent() && !focused && !hovered && !desk.motion.is_leaving(id);
    let instant = cx.reduce_motion() || !desk.prefs.animations;
    let depth = desk.motion.depth(id, recessed, instant, Instant::now());
    if depth > 0. && depth < 1. {
        window.request_animation_frame();
    }
    depth
}

fn controls_visible(desk: &Desk, id: &str, finding: bool, dropping: bool, leaving: bool) -> bool {
    if finding || dropping || leaving || desk.dragging_split().is_some() {
        return false;
    }
    desk.chrome_hover.as_deref() == Some(id)
        || desk.controls_hover.as_deref() == Some(id)
        || desk.pane_menu_open.as_deref() == Some(id)
}

/// Identity, recovery and the conversation action live in one quiet corner.
fn pane_corner(
    desk: &Desk,
    placement: &Placement,
    depth: f32,
    controls: bool,
    look: &Look,
    cx: &Context<Desk>,
) -> Option<gpui::Div> {
    if controls {
        return None;
    }
    let id = &placement.id;
    let corner_mark = desk
        .pane_identity(placement)
        .origin
        .harness()
        .filter(|_| depth > 0.)
        .map(|harness| harness_mark(harness, None, 14., look).opacity(0.85 * depth));
    let fenced = placement.terminal_id.as_ref().is_some_and(|id| {
        desk.link
            .as_ref()
            .is_some_and(|link| link.connection.fenced(id))
    });
    let conversation_toggle = desk.conversation_eligible(id) && !desk.conversation_visible(id);
    if corner_mark.is_none() && !fenced && !conversation_toggle {
        return None;
    }
    let conversation_id = id.clone();
    let warning = &look.chrome.theme.warning;
    Some(
        div()
            .absolute()
            .top(px(8.))
            .right(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .children(corner_mark)
            .when(fenced, |corner| {
                corner.child(pill(
                    "Typing may not have arrived",
                    &format!("{warning}22"),
                    warning,
                    look,
                ))
            })
            .when(conversation_toggle, |corner| {
                corner.child(
                    ghost_button(format!("conversation-open-{id}"), "Conversation").on_click(
                        cx.listener(move |desk, _, window, cx| {
                            desk.toggle_conversation(&conversation_id, window, cx);
                        }),
                    ),
                )
            }),
    )
}

/// Keep the desk field `slot` picks naming pane `id` while the pointer is over it.
pub(super) fn track_hover(
    id: String,
    slot: fn(&mut Desk) -> &mut Option<String>,
    cx: &mut Context<Desk>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    cx.listener(move |desk, hovered: &bool, _, cx| {
        let slot = slot(desk);
        if *hovered {
            *slot = Some(id.clone());
        } else if slot.as_deref() == Some(id.as_str()) {
            *slot = None;
        }
        cx.notify();
    })
}
