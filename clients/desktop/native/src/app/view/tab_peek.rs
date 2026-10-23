//! Live tab shelf above an unchanged, translated workspace.

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, MouseButton, div, px, relative};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use crate::workspace::layout::{Axis, DeskTab, LayoutNode, Placement};

use super::super::color::from_hex;
use super::super::desk::{self, Desk};
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::status::state_dot;

pub(super) fn height(available: f32) -> f32 {
    (available * 0.4).clamp(0., 216.)
}

pub(super) fn shelf(
    desk: &mut Desk,
    look: &Look,
    height: f32,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let tabs = desk
        .link
        .as_ref()
        .map(|link| link.workspace.tabs.clone())
        .unwrap_or_default();
    let live = tabs
        .iter()
        .flat_map(|tab| crate::workspace::layout::placements(&tab.root))
        .map(|placement| placement.id.as_str())
        .collect::<Vec<_>>();
    desk.tab_peek
        .scenes
        .retain(|id, _| live.contains(&id.as_str()));
    let scroll = desk.tab_peek.scroll.clone();
    div()
        .id("tab-peek")
        .debug_selector(|| "tab-peek".into())
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(height))
        .pl(px(super::panes::lead_inset(desk, look)))
        .pr(px(look.skin.shape.gutter))
        .py(px(8.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_xs()
                .text_color(from_hex(&look.chrome.muted))
                .child("Peek tabs")
                .child("← → browse · Enter opens · Esc closes"),
        )
        .child(
            div()
                .id("tab-peek-scroll")
                .debug_selector(|| "tab-peek-scroll".into())
                .flex_1()
                .min_h_0()
                .overflow_x_scroll()
                .track_scroll(&scroll)
                .flex()
                .gap(px(8.))
                .children(tabs.iter().map(|tab| card(desk, tab, look, cx))),
        )
        .into_any_element()
}

fn card(desk: &mut Desk, tab: &DeskTab, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let selected = desk.active_tab_id().as_deref() == Some(tab.id.as_str());
    let attention = super::tabs::tab_attention(desk, tab);
    let title = desk::tab_title(desk, tab);
    let preview = tree(desk, &tab.root, look);
    let id = tab.id.clone();
    let selector = format!("tab-peek-{}", tab.id);
    let hover = look.hover;
    div()
        .id(gpui::SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .w(px(256.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .surface(look, Surface::Pane)
        .border_color(if selected { look.lit } else { look.divider })
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(div().h(px(28.)).flex_shrink_0().px(px(8.)).flex().items_center().gap(px(6.))
            .child(Icon::new(IconName::SquareTerminal).size(px(12.)))
            .child(div().flex_1().min_w_0().truncate().text_xs().child(title))
            .child(state_dot(attention, None)))
        .child(div().flex_1().min_h_0().p(px(4.)).child(preview))
        // The entire miniature is navigation, never terminal mouse input.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |desk, _, window, cx| {
            desk.tab_peek.close();
            desk.select_tab(&id);
            desk.after_command(window, cx);
        }))
        .into_any_element()
}

fn tree(desk: &mut Desk, node: &LayoutNode, look: &Look) -> AnyElement {
    match node {
        LayoutNode::Leaf { placement } => {
            let id = placement.id.clone();
            div()
                .debug_selector(move || format!("tab-peek-pane-{id}"))
                .size_full()
                .overflow_hidden()
                .child(miniature(desk, placement, look))
                .into_any_element()
        }
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
            ..
        } => {
            let row = *axis == Axis::Row;
            div()
                .size_full()
                .flex()
                .gap(px(2.))
                .when(!row, |split| split.flex_col())
                .child(
                    div()
                        .flex_shrink_0()
                        .when(row, |side| side.w(relative(*ratio)).h_full())
                        .when(!row, |side| side.h(relative(*ratio)).w_full())
                        .child(tree(desk, first, look)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(tree(desk, second, look)),
                )
                .into_any_element()
        }
    }
}

fn miniature(desk: &mut Desk, placement: &Placement, look: &Look) -> AnyElement {
    let frame = placement
        .view_id
        .and_then(|view| desk.client()?.acquire_view(view));
    let settings = desk.terminal_settings(placement, false);
    let (Some(frame), Some(settings)) = (frame, settings) else {
        return div()
            .size_full()
            .bg(from_hex(&look.chrome.theme.background))
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(from_hex(&look.chrome.muted))
            .child("No terminal preview")
            .into_any_element();
    };
    desk.tab_peek
        .scenes
        .entry(placement.id.clone())
        .or_default()
        .element(frame, settings)
        .into_any_element()
}

/// Click the displaced work to close, consuming that click instead of sending
/// it into the terminal. The next click can interact with the restored work.
pub(super) fn dismiss(cx: &mut Context<Desk>) -> impl IntoElement {
    div()
        .id("tab-peek-dismiss")
        .absolute()
        .inset_0()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|desk, _, window, cx| {
                cx.stop_propagation();
                desk.dismiss_tab_peek(window, cx);
            }),
        )
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .block_mouse_except_scroll()
}
