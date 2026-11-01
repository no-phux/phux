//! The smallest pieces: secondary text, swatches, badges, lifts and highlights.

use gpui::prelude::*;
use gpui::{
    BoxShadow, Hsla, IntoElement, Keystroke, div, linear_color_stop, linear_gradient, point, px,
};

use super::super::color::from_hex;
use super::super::keys::gpui_stroke;
use super::super::skin::{Look, Surface, Surfaced};

/// Secondary text: extra-small, in the theme's muted ink.
pub(crate) fn muted(look: &Look, text: impl IntoElement) -> gpui::Div {
    div()
        .text_xs()
        .text_color(from_hex(&look.chrome.muted))
        .child(text)
}

/// A round swatch `size` points across.
pub(crate) fn dot(size: f32, color: Hsla) -> gpui::Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        .bg(color)
}

/// A small label chip, `color` text on `background`, both hex.
pub(crate) fn pill(text: &str, background: &str, color: &str, look: &Look) -> gpui::Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .rounded(px(look.skin.shape.kit_radius))
        .bg(from_hex(background))
        .text_xs()
        .text_color(from_hex(color))
        .child(text.to_owned())
}

/// A raised notice over the panes: the card surface on a near-opaque
/// backing, with the kit's large lift when the skin casts shadows.
pub(crate) fn notice_card(look: &Look, opacity: f32) -> gpui::Div {
    div()
        .surface(look, Surface::Card)
        .bg(from_hex(&look.chrome.raised).opacity(opacity))
        .when(!look.popover_shadow.is_empty(), |card| card.shadow_lg())
}

pub(crate) fn shadow(color: Hsla, y: f32, blur: f32, spread: f32, inset: bool) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset,
    }
}

/// Light catching a card's top edge: a hairline brightest at the centre,
/// `inset` points in from either side of the box it is placed in. Lay it on
/// the card's border row, not under it, or the edge paints two hairlines
/// thick. None when `color` is clear.
pub(crate) fn specular(color: Hsla, inset: f32) -> Option<gpui::Div> {
    let half = |from: Hsla, to: Hsla| {
        div().flex_1().h_full().bg(linear_gradient(
            90.,
            linear_color_stop(from, 0.),
            linear_color_stop(to, 1.),
        ))
    };
    (color.a > 0.).then(|| {
        div()
            .absolute()
            .top_0()
            .left(px(inset))
            .right(px(inset))
            .h(px(1.))
            .flex()
            .child(half(color.opacity(0.), color))
            .child(half(color, color.opacity(0.)))
    })
}

/// `chord` as a GPUI keystroke, when it parses.
pub(crate) fn keystroke(chord: &str) -> Option<Keystroke> {
    // gpui_stroke has already resolved host aliases to explicit modifiers.
    Keystroke::parse_with_os(&gpui_stroke(chord)?, gpui::OperatingSystem::Unknown).ok()
}
