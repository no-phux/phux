//! List rows: one shape for every navigator and list entry, so a session, a
//! tab and a terminal line up and read as the same kind of thing.

use gpui::prelude::*;
use gpui::{FontWeight, IntoElement, SharedString, div, px};

use crate::settings::Skin;

use super::super::color::from_hex;
use super::super::skin::Look;
use super::atoms::muted;

/// A row's minimum height.
pub(crate) const ROW: f32 = 30.;
/// A row's leading slot width.
pub(crate) const ICON: f32 = 16.;

/// List row corners: just inside the kit's.
pub(crate) fn row_radius(skin: &Skin) -> f32 {
    (skin.shape.kit_radius - 2.).max(0.)
}

/// How loudly a row's label reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Emphasis {
    /// What this window shows now.
    Current,
    Normal,
    /// Present but out of the way: background terminals, offline work.
    Quiet,
}

/// A row: leading slot, label, trailing detail; washed on hover.
pub(crate) fn row(id: impl Into<gpui::ElementId>, look: &Look) -> gpui::Stateful<gpui::Div> {
    let hover = look.hover;
    div()
        .id(id)
        .relative()
        .min_h(px(ROW))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(row_radius(&look.skin)))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
}

/// A row's centred leading slot.
pub(crate) fn row_icon(icon: impl IntoElement) -> gpui::Div {
    div()
        .relative()
        .w(px(ICON))
        .flex_shrink_0()
        .flex()
        .justify_center()
        .child(icon)
}

/// A row's truncating label.
pub(crate) fn row_label(
    text: impl Into<SharedString>,
    emphasis: Emphasis,
    look: &Look,
) -> gpui::Div {
    let (weight, color) = match emphasis {
        Emphasis::Current => (FontWeight::SEMIBOLD, look.ink),
        Emphasis::Normal => (FontWeight::MEDIUM, from_hex(&look.chrome.subtext)),
        Emphasis::Quiet => (FontWeight::NORMAL, from_hex(&look.chrome.muted)),
    };
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_sm()
        .font_weight(weight)
        .text_color(color)
        .child(text.into())
}

/// Faint trailing text on a row: where it runs, a count, a state word.
pub(crate) fn row_detail(text: impl Into<SharedString>, look: &Look) -> gpui::Div {
    div()
        .flex_shrink_0()
        .max_w(px(96.))
        .truncate()
        .text_xs()
        .text_color(from_hex(&look.chrome.faint))
        .child(text.into())
}

/// A quiet group heading.
pub(crate) fn section_label(title: &str, look: &Look) -> gpui::Div {
    muted(look, div().flex_1().truncate().child(title.to_owned()))
        .px(px(8.))
        .pt(px(10.))
        .h(px(28.))
        .flex()
        .items_center()
        .font_weight(FontWeight::MEDIUM)
}
