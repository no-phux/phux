//! A split at a glance: a tab's panes drawn as small cells in their own
//! proportions, so a tab shows where each pane sits before it is opened.

use gpui::prelude::*;
use gpui::{Hsla, div, px};

use crate::app::skin::Look;
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
    look: &Look,
) -> gpui::Div {
    let rim = look.ink.opacity(0.22);
    div()
        .relative()
        .flex_shrink_0()
        .w(px(width))
        .h(px(height))
        // The offset backplate gives the mosaic depth without hiding its
        // actual split shape or spending another slot on a generic stack icon.
        .child(
            div().absolute().top_0().right_0()
                .w(px(width - 3.)).h(px(height - 3.))
                .rounded(px(3.)).border_1().border_color(rim)
                .bg(look.ink.opacity(0.05)),
        )
        .child(
            div().absolute().left_0().bottom_0()
                .w(px(width - 3.)).h(px(height - 3.))
                .rounded(px(3.)).border_1().border_color(rim)
                .bg(look.sheet.opacity(1.)).p(px(2.))
                .flex().child(cells(node, paint)),
        )
}

fn cells(node: &LayoutNode, paint: &dyn Fn(&Placement) -> Hsla) -> gpui::Div {
    match node {
        LayoutNode::Leaf { placement } => {
            let id = placement.id.clone();
            div()
                .debug_selector(move || format!("layout-cell-{id}"))
                .size_full()
                .rounded(px(1.))
                .bg(paint(placement))
        }
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
                        .flex_basis(px(0.))
                        .flex_grow(*ratio)
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .child(cells(first, paint)),
                )
                .child(
                    div()
                        .flex_basis(px(0.))
                        .flex_grow(1. - *ratio)
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .child(cells(second, paint)),
                )
        }
    }
}
