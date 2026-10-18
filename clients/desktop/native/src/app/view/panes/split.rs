//! The split tree and the dividers between its panes.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, IntoElement, MouseButton, ParentElement, SharedString, Styled, Window,
    div, px, relative,
};

use crate::workspace::layout::{Axis, LayoutNode};

use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::motion;
use crate::app::skin::Look;

/// The narrowest grab area a split keeps when its painted gap is thinner.
const SPLIT_GRAB: f32 = 8.;

/// How present a split child is: a leaf follows its motion, a subtree is settled.
fn node_presence(desk: &Desk, node: &LayoutNode, now: Instant) -> f32 {
    match node {
        LayoutNode::Leaf { placement } => desk.motion.presence(&placement.id, now),
        LayoutNode::Split { .. } => 1.,
    }
}

pub(super) fn split_tree(
    desk: &mut Desk,
    node: &LayoutNode,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    match node {
        LayoutNode::Leaf { placement } => super::pane_view(desk, placement, window, cx),
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
