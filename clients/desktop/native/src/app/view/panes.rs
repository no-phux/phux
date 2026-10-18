//! The workspace: the active tab's split tree of pane cards, its dividers,
//! each pane's floating controls, and the empty state.
//!
//! A pane is a rounded card on the glass with no header. Its controls float
//! in when the pointer reaches the top edge, and a file dragged over it shows
//! where the drop will paste.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Context, DragMoveEvent, ExternalPaths, FileDropEvent,
    Hsla, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window, div, px, relative,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::kbd::Kbd;
use phux_client_runtime::control::Status;

use crate::connection::AgentInfo;
use crate::workspace::layout::{self, Axis, LayoutNode, Placement};

use super::super::color::from_hex;
use super::super::desk::Desk;
use super::super::motion;
use super::super::overlays;
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::menu::Anchor;
use super::super::ui::{
    agent_swatch, dot, ghost_button, ghost_icon, keystroke, muted, notice_card, pill, shadow,
    specular,
};
use super::gloss;

/// How far down a pane the pointer reveals its controls.
const CONTROLS_EDGE: f32 = 36.;
/// The narrowest grab area a split keeps when its painted gap is thinner.
const SPLIT_GRAB: f32 = 8.;

/// A pane card's shadow plus a faint inner rim while the skin is glassy.
fn pane_shadows(look: &Look) -> Vec<BoxShadow> {
    let mut shadows = look.pane_shadow.clone();
    let rim = look.rim.opacity(0.6 * gloss(&look.skin));
    if rim.a > 0. {
        shadows.push(shadow(rim, 0., 0., 1., true));
    }
    shadows
}

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

fn empty_state(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.link.as_ref().map(|link| link.connection.status);
    let error = desk.connect_error.clone().or_else(|| {
        desk.link
            .as_ref()
            .and_then(|link| link.connection.error.clone())
    });
    let pending = desk
        .link
        .as_ref()
        .is_some_and(|link| link.workspace.pending() > 0);
    let new = if pending {
        "Starting terminal…"
    } else {
        "New terminal"
    };
    let body = match status {
        Some(Status::Attached) => div().flex().flex_col().items_center().gap(px(8.)).children(
            [
                ("empty-new", new, "new", true),
                ("empty-folder", "Open folder…", "folder", false),
                ("empty-palette", "Command palette", "palette", false),
            ]
            .map(|(id, label, command, primary)| {
                empty_action(desk, id, label, command, primary, cx)
            }),
        ),
        Some(Status::Idle | Status::Connecting | Status::Negotiated) => muted(
            look,
            match desk.remote.as_deref() {
                Some(host) => format!("Connecting to {host}…"),
                None => format!("Connecting to {}…", desk.socket.display()),
            },
        )
        .text_sm(),
        _ => div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .max_w(px(520.))
                    .text_center()
                    .text_sm()
                    .text_color(from_hex(&look.chrome.theme.danger))
                    .child(error.unwrap_or_else(|| "The connection closed.".into())),
            )
            .child(empty_action(
                desk,
                "empty-reconnect",
                "Reconnect",
                "reconnect",
                true,
                cx,
            )),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(18.))
        .child(
            div()
                .text_size(px(34.))
                .font_weight(gpui::FontWeight::EXTRA_BOLD)
                .child("phux"),
        )
        .child(body)
}

fn empty_action(
    desk: &Desk,
    id: &'static str,
    label: &str,
    command: &'static str,
    primary: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    let kbd = command_kbd(desk, command);
    let button = Button::new(id)
        .label(label.to_owned())
        .w(px(240.))
        .on_click(cx.listener(move |desk, _, window, cx| desk.execute(command, window, cx)));
    let button = if primary { button.primary() } else { button };
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(button)
        .child(div().w(px(64.)).children(kbd))
}

fn command_kbd(desk: &Desk, id: &str) -> Option<Kbd> {
    keystroke(desk.bound_chord(id)?).map(Kbd::new)
}

/// How present a split child is: a leaf follows its motion, a subtree is settled.
fn node_presence(desk: &Desk, node: &LayoutNode, now: Instant) -> f32 {
    match node {
        LayoutNode::Leaf { placement } => desk.motion.presence(&placement.id, now),
        LayoutNode::Split { .. } => 1.,
    }
}

fn split_tree(
    desk: &mut Desk,
    node: &LayoutNode,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    match node {
        LayoutNode::Leaf { placement } => pane_view(desk, placement, window, cx),
        LayoutNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let horizontal = matches!(axis, Axis::Row);
            let bounds = desk.split_bounds.clone();
            let key = id.clone();
            let now = Instant::now();
            let (lead, trail) = (
                node_presence(desk, first, now),
                node_presence(desk, second, now),
            );
            let painted = motion::ratio(*ratio, lead, trail);
            let shape = &look.skin.shape;
            let gap = shape.gap * lead.min(trail);
            let thick = gap.max(shape.divider);
            // Too thin to grab: the second side lends a wider strip over the line.
            let lend = thick < SPLIT_GRAB;
            let reach = -(SPLIT_GRAB + thick) / 2.;
            div()
                .size_full()
                .flex()
                .when(horizontal, |row| row.flex_row())
                .when(!horizontal, |column| column.flex_col())
                .child(
                    div()
                        .when(horizontal, |side| side.w(relative(painted)).h_full())
                        .when(!horizontal, |side| side.h(relative(painted)).w_full())
                        .flex_shrink_0()
                        .child(split_tree(desk, first, look, window, cx)),
                )
                .child(split_divider(desk, id.clone(), *axis, gap, thick, look, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .when(lend, |side| side.relative())
                        .child(split_tree(desk, second, look, window, cx))
                        .when(lend, |side| {
                            let strip = div().absolute();
                            let strip = if horizontal {
                                strip.top_0().bottom_0().left(px(reach)).w(px(SPLIT_GRAB))
                            } else {
                                strip.left_0().right_0().top(px(reach)).h(px(SPLIT_GRAB))
                            };
                            side.child(split_grab(strip, id.clone(), *axis, cx))
                        }),
                )
                .on_children_prepainted(move |children, _, _| {
                    if let (Some(first), Some(last)) = (children.first(), children.last()) {
                        bounds.borrow_mut().insert(key.clone(), first.union(last));
                    }
                })
                .into_any_element()
        }
    }
}

/// The sheet between two panes, `thick` points across. A skin with a gap
/// grabs it whole and shows a short pill on hover, in the accent while
/// dragged; a skin with a painted divider draws the line, and the second
/// side lends it a wider grab strip.
fn split_divider(
    desk: &Desk,
    id: String,
    axis: Axis,
    gap: f32,
    thick: f32,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let horizontal = matches!(axis, Axis::Row);
    let active = desk.dragging_split() == Some(id.as_str());
    let accent = from_hex(&look.chrome.theme.accent);
    let grip = look.ink.opacity(0.28);
    let divider = look.skin.shape.divider;
    let bar = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .when(horizontal, |bar| bar.w(px(thick)).h_full())
        .when(!horizontal, |bar| bar.h(px(thick)).w_full())
        .when(divider > 0., |bar| {
            // The line takes over the bar as the gap closes onto it.
            bar.bg(if active {
                accent
            } else {
                look.divider.opacity(divider.min(1.) * divider / thick)
            })
        });
    if thick < SPLIT_GRAB {
        return bar.into_any_element();
    }
    split_grab(bar.group("divider"), id, axis, cx)
        .when(gap > divider, |bar| {
            bar.child(
                div()
                    .rounded_full()
                    .when(active, |pill| pill.bg(accent))
                    .when(!active, |pill| {
                        pill.group_hover("divider", move |style| style.bg(grip))
                    })
                    .when(horizontal, |pill| pill.w(px(3.)).h(px(44.)))
                    .when(!horizontal, |pill| pill.h(px(3.)).w(px(44.))),
            )
        })
        .into_any_element()
}

/// Make `area` start a drag of split `id`.
fn split_grab(
    area: gpui::Div,
    id: String,
    axis: Axis,
    cx: &mut Context<Desk>,
) -> gpui::Stateful<gpui::Div> {
    let horizontal = matches!(axis, Axis::Row);
    area.id(SharedString::from(format!("split-{id}")))
        .when(horizontal, |area| area.cursor_col_resize())
        .when(!horizontal, |area| area.cursor_row_resize())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |desk, _, _, cx| {
                desk.begin_split_drag(id.clone(), axis, cx);
                cx.stop_propagation();
            }),
        )
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
    let terminal = placement.terminal_id.clone();
    let view = placement.view_id;
    // The terminal holds the keyboard only while no modal or find field does.
    let settings = desk.terminal_settings(placement, focused && !desk.text_field_has_keys());
    let client = desk.client();
    desk.prepare_pending_native(&placement_id, window, cx);
    let body = if desk.conversation_visible(&placement_id) {
        super::super::conversation::render(desk, &placement_id, colors, window, cx)
    } else if let (Some(terminal), Some(view), Some(settings), Some(client)) =
        (terminal, view, settings, client)
    {
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
    } else {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(from_hex(&colors.muted))
            .child(if placement.terminal_id.is_none() {
                "Saved terminal is not on this server"
            } else {
                "Waiting for terminal"
            })
            .into_any_element()
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
    let dim = (!focused && shared)
        .then_some(1. - desk.prefs.unfocused_opacity as f32)
        .filter(|amount| *amount > 0.);
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
    let id = placement.id.clone();
    let dropping = desk.drop_target.as_deref() == Some(id.as_str());
    let remote_files = placement.terminal_id.as_ref().is_some_and(|terminal| {
        !matches!(
            crate::attach::terminal_host(desk.remote.as_deref(), terminal),
            crate::attach::TerminalHost::Local
        )
    });
    let leaving = desk.motion.is_leaving(&id);
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
        .opacity(if focused { 0.7 } else { 0.35 } * gloss(&look.skin));
    let controls_bar =
        controls.then(|| pane_controls(desk, placement, zoomed, &look, cx).into_any_element());
    let conversation_toggle =
        desk.conversation_eligible(&id) && !desk.conversation_visible(&id) && !controls;
    let conversation_id = id.clone();
    let drag_id = id.clone();
    let drop_id = id.clone();
    let exit_id = id.clone();
    div()
        .id(SharedString::from(format!("pane-card-{id}")))
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .when(presence < 1., |card| card.opacity(presence.max(0.)))
        .surface(&look, Surface::Pane)
        .border_color(edge)
        .shadow(pane_shadows(&look))
        .overflow_hidden()
        .block_mouse_except_scroll()
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
                .px(px(desk.prefs.padding_x as f32))
                .py(px(desk.prefs.padding_y as f32))
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
        .when(fenced && !controls, |card| {
            card.child(div().absolute().top(px(8.)).right(px(10.)).child(pill(
                "Typing may not have arrived",
                &format!("{}22", colors.theme.warning),
                &colors.theme.warning,
                &look,
            )))
        })
        .children(controls_bar)
        .when(conversation_toggle, |card| {
            card.child(div().absolute().top(px(8.)).right(px(10.)).child(
                ghost_button(format!("conversation-open-{id}"), "Conversation").on_click(
                    cx.listener(move |desk, _, window, cx| {
                        desk.toggle_conversation(&conversation_id, window, cx);
                    }),
                ),
            ))
        })
        .when(dropping, |card| card.child(drop_hint(&look, remote_files)))
        .children(find_bar)
        .into_any_element()
}

/// The floating capsule a pane shows at its top edge: what runs there, then
/// split, zoom and close.
fn pane_controls(
    desk: &Desk,
    placement: &Placement,
    zoomed: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let title = desk.pane_title(placement.terminal_id.as_ref());
    let agent = placement
        .terminal_id
        .as_ref()
        .and_then(|id| desk.agent(id))
        .cloned();
    let id = placement.id.clone();
    let focus_id = placement.id.clone();
    let zoom = on_pane(id.clone(), "zoom");
    let zoom_label = if zoomed { "Restore" } else { "Zoom" };
    div()
        .id(SharedString::from(format!("pane-controls-{id}")))
        .absolute()
        .top(px(7.))
        .right(px(8.))
        .max_w(relative(0.9))
        .h(px(28.))
        .pl(px(10.))
        .pr(px(3.))
        .flex()
        .items_center()
        .gap(px(6.))
        .surface(look, Surface::Capsule)
        .occlude()
        .text_xs()
        .on_hover(track_hover(id.clone(), |desk| &mut desk.controls_hover, cx))
        .children(
            agent
                .as_ref()
                .map(|info| dot(7., from_hex(agent_swatch(info.state)))),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(look.ink)
                .child(match agent.as_ref().map(|info| (info, info.word())) {
                    Some((info, "")) => info.name.clone(),
                    Some((info, word)) => format!("{} · {word}", info.name),
                    None => title,
                }),
        )
        .child(div().w(px(2.)))
        .when(desk.conversation_eligible(&id), |row| {
            let conversation_id = id.clone();
            row.child(
                ghost_button(
                    format!("pane-conversation-{id}"),
                    if desk.conversation_visible(&id) {
                        "Terminal"
                    } else {
                        "Conversation"
                    },
                )
                .on_click(cx.listener(move |desk, _, window, cx| {
                    cx.stop_propagation();
                    desk.toggle_conversation(&conversation_id, window, cx);
                })),
            )
        })
        .child(
            ghost_icon(
                format!("zoom-{id}"),
                if zoomed {
                    IconName::Minimize
                } else {
                    IconName::Maximize
                },
                zoom_label,
            )
            .on_click(cx.listener(move |desk, _, window, cx| {
                cx.stop_propagation();
                zoom(desk, window, cx);
            })),
        )
        .child({
            let placement = id.clone();
            let selector = id.clone();
            div()
                .relative()
                .debug_selector(move || format!("pane-actions-{selector}"))
                .child(desk.anchors.probe(format!("pane-actions-{id}")))
                .child(
                    ghost_icon(
                        format!("pane-more-{id}"),
                        IconName::Ellipsis,
                        "Pane actions",
                    )
                    .on_click(cx.listener(
                        move |desk, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            let anchor = desk
                                .anchors
                                .get(&format!("pane-actions-{placement}"))
                                .map_or(Anchor::At(event.position()), Anchor::Below);
                            desk.open_pane_menu(&placement, anchor, window, cx);
                        },
                    )),
                )
        })
        .on_click(cx.listener(move |desk, event: &ClickEvent, window, cx| {
            if let Some(tab) = desk.active_tab_id() {
                desk.focus_placement(&tab, &focus_id, window, cx);
            }
            if event.click_count() >= 2 {
                desk.execute("zoom", window, cx);
            }
        }))
}

/// Shown over the pane a file drag from the OS is above.
fn drop_hint(look: &Look, remote: bool) -> impl IntoElement {
    let accent = from_hex(&look.chrome.theme.accent);
    let (title, detail) = if remote {
        (
            "Drop to upload",
            "Sent to that machine, then its path is pasted",
        )
    } else {
        ("Drop to attach", "Paths paste here, ready for your agent")
    };
    div()
        .absolute()
        .inset_0()
        .rounded(px(look.skin.shape.radius))
        .bg(accent.opacity(0.1))
        .border_2()
        .border_color(accent.opacity(0.8))
        .flex()
        .items_center()
        .justify_center()
        .child(
            notice_card(look, 0.92)
                .px(px(16.))
                .py(px(10.))
                .rounded(px(look.skin.shape.card_radius - 2.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(look.ink)
                        .child(title),
                )
                .child(muted(look, detail)),
        )
}

/// Focus this pane, then run `command` on it.
fn on_pane(
    placement: String,
    command: &'static str,
) -> impl Fn(&mut Desk, &mut Window, &mut Context<Desk>) + 'static {
    move |desk, window, cx| {
        if let Some(tab) = desk.active_tab_id() {
            desk.focus_placement(&tab, &placement, window, cx);
        }
        desk.execute(command, window, cx);
    }
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
