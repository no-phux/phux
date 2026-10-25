//! Live tab shelf above an unchanged, translated workspace.

use gpui::prelude::*;
use gpui::{
    AnyElement, AvailableSpace, Bounds, ContentMask, Context, FontWeight, IntoElement, MouseButton,
    ScrollHandle, canvas, div, point, px, relative, size,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use crate::workspace::layout::{Axis, DeskTab, LayoutNode, Placement};

use super::super::color::from_hex;
use super::super::desk::{self, Desk};
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::ghost_icon;
use crate::settings::TabPosition;

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
    // The titlebar is prepainted first. Follow its actual bounds in this same
    // frame, including sidebar springs and scrolling, not a cached approximation.
    // A side panel can narrow the work independently of the titlebar; use the
    // readable overview there, as with bottom and sidebar tabs.
    let aligned = super::tabs_at(desk, &look.skin, TabPosition::Top) && !desk.changes.open;
    let cards = tabs.iter().map(|tab| card(desk, tab, look, cx)).collect();
    let previews = if aligned {
        aligned_row(cards, desk.tab_peek.strip_scroll.clone(), cx)
    } else {
        overview_row(cards, &scroll)
    };
    div()
        .id("tab-peek")
        .debug_selector(|| "tab-peek".into())
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(height))
        .pt(px(8.))
        .pb(px(4.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .when(!aligned, |shelf| {
            shelf
                .pl(px(super::panes::lead_inset(desk, look)))
                .pr(px(look.skin.shape.gutter))
        })
        .child(previews)
        .child(
            div()
                .h(px(24.))
                .flex_shrink_0()
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_xs()
                .text_color(from_hex(&look.chrome.muted))
                .child(div().flex_shrink_0().child("Tab previews"))
                .child(div().flex_1())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child("← → Browse · ↵ Open · Esc Close"),
                )
                .child(div().debug_selector(|| "close-tab-peek".into()).child(
                    ghost_icon("close-tab-peek", IconName::X, "Close previews").on_click(
                        cx.listener(|desk, _, window, cx| {
                            desk.dismiss_tab_peek(window, cx);
                        }),
                    ),
                )),
        )
        .into_any_element()
}

/// No second horizontal grid: lay out each card at its owning tab's width,
/// then paint directly below it inside the same scroll viewport.
fn aligned_row(cards: Vec<AnyElement>, scroll: ScrollHandle, cx: &mut Context<Desk>) -> AnyElement {
    let wheel_scroll = scroll.clone();
    div()
        .id("tab-peek-scroll")
        .debug_selector(|| "tab-peek-scroll".into())
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .child(
            canvas(
                move |bounds, window, cx| {
                    let strip = scroll.bounds();
                    let left = bounds.left().max(strip.left()).min(bounds.right());
                    let mask = ContentMask {
                        bounds: Bounds::from_corners(
                            point(left, bounds.top()),
                            point(bounds.right().min(strip.right()).max(left), bounds.bottom()),
                        ),
                    };
                    let offset = scroll.offset();
                    let mut painted = Vec::with_capacity(cards.len());
                    window.with_content_mask(Some(mask), |window| {
                        for (index, mut card) in cards.into_iter().enumerate() {
                            let Some(tab) = scroll.bounds_for_item(index) else {
                                continue;
                            };
                            card.layout_as_root(
                                size(
                                    AvailableSpace::Definite(tab.size.width),
                                    AvailableSpace::Definite(bounds.size.height),
                                ),
                                window,
                                cx,
                            );
                            card.prepaint_at(
                                point(tab.left() + offset.x, bounds.top()),
                                window,
                                cx,
                            );
                            painted.push(card);
                        }
                    });
                    (mask, painted)
                },
                |_, (mask, mut cards), window, cx| {
                    window.with_content_mask(Some(mask), |window| {
                        for card in &mut cards {
                            card.paint(window, cx);
                        }
                    });
                },
            )
            .size_full(),
        )
        .on_scroll_wheel(
            cx.listener(move |_, event: &gpui::ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(20.));
                let delta = if delta.x == px(0.) { delta.y } else { delta.x };
                let x =
                    (wheel_scroll.offset().x + delta).clamp(-wheel_scroll.max_offset().x, px(0.));
                wheel_scroll.set_offset(point(x, px(0.)));
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .into_any_element()
}

fn overview_row(cards: Vec<AnyElement>, scroll: &ScrollHandle) -> AnyElement {
    div()
        .id("tab-peek-scroll")
        .debug_selector(|| "tab-peek-scroll".into())
        .flex_1()
        .min_h_0()
        .overflow_x_scroll()
        .track_scroll(scroll)
        .flex()
        .gap(px(8.))
        .children(
            cards
                .into_iter()
                .map(|card| div().w(px(256.)).h_full().flex_shrink_0().child(card)),
        )
        .into_any_element()
}

fn card(desk: &mut Desk, tab: &DeskTab, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let selected = desk.active_tab_id().as_deref() == Some(tab.id.as_str());
    let title = desk::tab_title(desk, tab);
    let lead = super::tabs::side_lead(
        super::tabs::tab_attention(desk, tab),
        super::tabs::tab_agent(desk, tab),
        desk.tab_harness(tab),
        selected,
        look,
    );
    let preview = tree(desk, &tab.root, look);
    let id = tab.id.clone();
    let selector = format!("tab-peek-{}", tab.id);
    let hover = look.hover;
    let lit = look.lit;
    let accent = from_hex(&look.chrome.theme.accent);
    div()
        .id(gpui::SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .size_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .surface(look, Surface::Pane)
        .border_color(if selected { accent } else { look.divider })
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |style| style.border_color(if selected { accent } else { lit }).bg(hover))
        .child(div().h(px(28.)).flex_shrink_0().px(px(8.)).flex().items_center().gap(px(6.))
            .when(selected, |header| header.bg(look.active))
            .child(lead)
            .child(div().flex_1().min_w_0().truncate().text_xs()
                .when(selected, |label| label.font_weight(FontWeight::MEDIUM)).child(title))
            .child(Icon::new(IconName::Check).size(px(12.)).text_color(accent)
                .when(!selected, |mark| mark.invisible())))
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
