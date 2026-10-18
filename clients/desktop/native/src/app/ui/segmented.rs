//! A segmented choice: a few mutually exclusive options in one track.

use gpui::prelude::*;
use gpui::{App, ClickEvent, SharedString, Window, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};

use super::super::skin::Look;

/// One option: its value, label, icon, and debug selector.
pub(crate) struct Segment<T> {
    pub value: T,
    pub label: &'static str,
    pub icon: IconName,
    pub selector: String,
}

/// `segments` in one track, `selected` raised. `choose` makes each option's
/// click handler.
pub(crate) fn segmented<T: Copy + PartialEq + 'static, F>(
    look: &Look,
    selected: T,
    segments: impl IntoIterator<Item = Segment<T>>,
    choose: impl Fn(T) -> F,
) -> gpui::Div
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    // Track and thumb from the skin's control radius: 10 and 7 on glass.
    let radius = look.skin.shape.kit_radius;
    let ink = look.ink;
    div()
        .tab_group()
        .flex()
        .gap(px(3.))
        .p(px(3.))
        .rounded(px(radius + 2.))
        .bg(ink.opacity(0.05))
        .children(segments.into_iter().map(|segment| {
            let on = segment.value == selected;
            let selector = segment.selector;
            div().debug_selector(|| selector.clone()).child(
                Button::new(SharedString::from(selector.clone()))
                    .label(segment.label)
                    .icon(segment.icon)
                    .small()
                    .ghost()
                    .toggled(on)
                    .rounded(px((radius - 1.).max(0.)))
                    .bg(ink.opacity(if on { 0.10 } else { 0. }))
                    .text_color(ink.opacity(if on { 1. } else { 0.65 }))
                    .on_click(choose(segment.value)),
            )
        }))
}
