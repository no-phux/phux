//! The workspace: the active tab's split tree of pane cards, its dividers,
//! each pane's floating controls, and the empty state.
//!
//! A pane is a rounded card on the glass with no header. Its controls float
//! in when the pointer reaches the top edge, and a file dragged over it shows
//! where the drop will paste.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DragMoveEvent, ExternalPaths, FileDropEvent, Hsla, IntoElement,
    ParentElement, SharedString, Styled, Window, div, px,
};

use crate::connection::AgentInfo;
use crate::workspace::layout::{self, Placement};

use super::super::color::from_hex;
use super::super::desk::Desk;
use super::super::overlays;
use super::super::pane_identity::PaneKind;
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::status::harness_mark;
use super::super::ui::{agent_swatch, ghost_button, pill, specular};
use super::gloss;

mod controls;
mod depth;
mod empty;
mod split;

use controls::{drop_hint, pane_controls};
use depth::{depth_inset, pane_shadows, veils};
use empty::empty_state;
use split::split_tree;

/// How far down a pane the pointer reveals its controls.
const CONTROLS_EDGE: f32 = 36.;

pub(super) fn main_column(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let status = look.skin.chrome.status;
    div()
        .flex_grow(1.)
        .min_w(px(0.))
        .flex()
        .flex_col()
        .child(workspace_body(desk, look, window, cx))
        .when(status > 0., |column| {
            column.child(super::status::status_bar(desk, status, look, cx))
        })
}

fn workspace_body(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let gutter = look.skin.shape.gutter;
    let zoomed = desk.active_tab().and_then(|tab| tab.zoomed_id.clone());
    let root = desk.active_tab().map(|tab| tab.root.clone());
    div()
        .flex_grow(1.)
        .min_h(px(0.))
        .relative()
        .pt(px(gutter))
        .px(px(gutter))
        .child(if let Some(id) = zoomed {
            desk.placement(&id).cloned().map_or_else(
                || div().into_any_element(),
                |placement| pane_view(desk, &placement, window, cx),
            )
        } else if let Some(root) = root {
            split_tree(desk, &root, look, window, cx).into_any_element()
        } else {
            empty_state(desk, look, cx).into_any_element()
        })
        .children(super::status::peek_card(desk, look, cx))
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
    let (Some(terminal), Some(view), Some(settings), Some(client)) = (
        placement.terminal_id.clone(),
        placement.view_id,
        settings,
        client,
    ) else {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(from_hex(&look.chrome.muted))
            .child(if placement.terminal_id.is_none() {
                "Saved terminal is not on this server"
            } else {
                "Waiting for terminal"
            })
            .into_any_element();
    };
    let remote = desk.remote.clone();
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
    div()
        .size_full()
        .child(entity.cached(gpui::StyleRefinement::default().size_full()))
        .into_any_element()
}

/// One pane: a rounded card on the glass, with no header. Its controls float
/// in when the pointer reaches the top edge, and a file dragged over it shows
/// where the drop will paste.
fn pane_view(
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
        PaneKind::Conversation => {
            super::super::conversation::render(desk, &placement_id, colors, window, cx)
        }
        PaneKind::Terminal => terminal_body(desk, placement, focused, &look, window, cx),
    };
    let tab = desk.active_tab();
    let zoomed = tab.is_some_and(|tab| tab.zoomed_id.is_some());
    // Focus only shows among several visible panes.
    let shared = !zoomed && tab.is_some_and(|tab| layout::placements(&tab.root).len() > 1);
    let find = desk
        .find
        .as_ref()
        .filter(|find| find.placement_id == placement.id)
        .map(|find| find.case_sensitive);
    let find_bar = find.map(|case| overlays::find_bar(desk, case, cx));
    let id = placement.id.clone();
    let leaving = desk.motion.is_leaving(&id);
    // An agent's pane sits back until it holds focus or the pointer.
    let hovered = desk.motion.hover.as_deref() == Some(id.as_str());
    let recessed = identity.origin.is_agent() && !focused && !hovered && !leaving;
    let instant = cx.reduce_motion() || !desk.prefs.animations;
    let depth = desk.motion.depth(&id, recessed, instant, Instant::now());
    if depth > 0. && depth < 1. {
        window.request_animation_frame();
    }
    let unfocused = if !focused && shared {
        1. - desk.prefs.unfocused_opacity as f32
    } else {
        0.
    };
    let dim =
        Some(veils(unfocused, look.skin.material.depth.veil * depth)).filter(|amount| *amount > 0.);
    let (padding_x, padding_y) = (desk.prefs.padding_x as f32, desk.prefs.padding_y as f32);
    let (inset_x, inset_y) = depth_inset(&look, depth, padding_x, padding_y);
    let fade = from_hex(&colors.theme.background);
    let agent = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.agent(id))
        .cloned();
    let urgent = agent.as_ref().is_some_and(AgentInfo::urgent);
    let fenced = placement.terminal_id.as_ref().is_some_and(|id| {
        desk.link
            .as_ref()
            .is_some_and(|link| link.connection.fenced(id))
    });
    let dropping = desk.drop_target.as_deref() == Some(id.as_str());
    let remote_files = placement.terminal_id.as_ref().is_some_and(|terminal| {
        !matches!(
            crate::attach::terminal_host(desk.remote.as_deref(), terminal),
            crate::attach::TerminalHost::Local
        )
    });
    let controls = find.is_none()
        && !dropping
        && !leaving
        && desk.dragging_split().is_none()
        && (desk.chrome_hover.as_deref() == Some(id.as_str())
            || desk.controls_hover.as_deref() == Some(id.as_str())
            || desk.pane_menu_open.as_deref() == Some(id.as_str()));
    let edge = if dropping {
        from_hex(&colors.theme.accent)
    } else if urgent {
        agent
            .as_ref()
            .map(|info| from_hex(agent_swatch(info.state)).opacity(0.75))
            .unwrap_or(look.lit)
    } else if focused && shared {
        look.lit
    } else {
        gpui::transparent_black()
    };
    let shine = look
        .lit
        .opacity(if focused { 0.7 } else { 0.35 } * gloss(&look.skin) * (1. - depth));
    let controls_bar = controls.then(|| {
        pane_controls(desk, placement, identity.origin, zoomed, &look, cx).into_any_element()
    });
    let conversation_toggle =
        desk.conversation_eligible(&id) && !desk.conversation_visible(&id) && !controls;
    // While it sits back, the pane names its harness in the corner; forward,
    // the controls capsule does.
    let corner_mark = identity
        .origin
        .harness()
        .filter(|_| depth > 0. && !controls)
        .map(|harness| {
            let state = agent.as_ref().map(|info| info.state);
            harness_mark(harness, state, 14., &look).opacity(0.85 * depth)
        });
    let corner =
        (corner_mark.is_some() || (fenced && !controls) || conversation_toggle).then(|| {
            let conversation_id = id.clone();
            div()
                .absolute()
                .top(px(8.))
                .right(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .children(corner_mark)
                .when(fenced && !controls, |corner| {
                    corner.child(pill(
                        "Typing may not have arrived",
                        &format!("{}22", colors.theme.warning),
                        &colors.theme.warning,
                        &look,
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
                })
        });
    let drag_id = id.clone();
    let drop_id = id.clone();
    let exit_id = id.clone();
    let card = div()
        .id(SharedString::from(format!("pane-card-{id}")))
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .when(presence < 1., |card| card.opacity(presence.max(0.)))
        .surface(&look, Surface::Pane)
        .border_color(edge)
        .shadow(pane_shadows(&look, depth))
        .overflow_hidden()
        .block_mouse_except_scroll()
        .on_hover(track_hover(id.clone(), |desk| &mut desk.motion.hover, cx))
        .on_drag_move::<ExternalPaths>(cx.listener(
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
        .children(specular(shine, look.skin.shape.radius))
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
        .children(find_bar);
    // The slot keeps the split's geometry; the card draws in from its edges.
    div()
        .size_full()
        .px(px(inset_x))
        .py(px(inset_y))
        .child(card)
        .into_any_element()
}

/// Keep the desk field `slot` picks naming pane `id` while the pointer is over it.
fn track_hover(
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
