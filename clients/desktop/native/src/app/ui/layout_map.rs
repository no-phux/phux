//! A split at a glance: a tab's panes drawn as small cells in their own
//! proportions, so a tab shows where each pane sits before it is opened.

use gpui::prelude::*;
use gpui::{Hsla, div, px, relative};

use crate::workspace::layout::{Axis, LayoutNode, Placement};

/// The space between two cells.
const CELL_GAP: f32 = 1.5;

/// `node` as cells in a `width` by `height` frame, each pane painted
/// `paint` returns for it.
pub(crate) fn layout_map(
    node: &LayoutNode,
    width: f32,
    height: f32,
    paint: &dyn Fn(&Placement) -> Hsla,
) -> gpui::Div {
    div()
        .flex_shrink_0()
        .w(px(width))
        .h(px(height))
        .flex()
        .child(cells(node, paint))
}

fn cells(node: &LayoutNode, paint: &dyn Fn(&Placement) -> Hsla) -> gpui::Div {
    match node {
        LayoutNode::Leaf { placement } => div().size_full().rounded(px(2.)).bg(paint(placement)),
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            let row = matches!(axis, Axis::Row);
            div()
                .size_full()
                .flex()
                .when(!row, |split| split.flex_col())
                .gap(px(CELL_GAP))
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .when(row, |side| side.w(relative(*ratio)).h_full())
                        .when(!row, |side| side.h(relative(*ratio)).w_full())
                        .child(cells(first, paint)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .child(cells(second, paint)),
                )
        }
    }
}
