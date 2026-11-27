//! The one grid Sessions and Terminal actions share, as an iOS inset
//! grouped list lays out: cards on the page margin, content inset inside
//! them, a leading column every mark is centred in, one text column, and a
//! trailing column for chevrons, checks and the search field's clear.
//!
//! At 393 points and the default text size: cards 16–377, section titles
//! and the leading column from 32, marks centred on 44, text from 64,
//! the trailing column 345–361.
use rune::{Color, El, el};

use super::Look;
use crate::desk::accessibility::GRID_CAP;
use crate::ui::tokens::Paint;

/// The page margin: title, search, every card and the dock start here.
pub(super) const fn margin() -> f32 {
    crate::ui::tokens::phone::page_margin()
}

/// Content inside a card, and a section's title over it, starts this far
/// in from the card's edge; trailing marks end as far from the other.
pub(super) fn inset(p: &Look) -> f32 {
    p.space.s4
}

/// Marks grow with the text beside them, up to the grid's cap.
fn mark_scale(p: &Look) -> f32 {
    p.type_scale.min(GRID_CAP)
}

/// A leading mark: a pane's identity, an action's icon, the magnifier.
pub(super) fn glyph(p: &Look) -> f32 {
    p.icon.grid * mark_scale(p)
}

/// The leading column's width.
pub(super) fn lead_w(p: &Look) -> f32 {
    (p.space.s5 + p.space.s1) * mark_scale(p)
}

/// Where titles, labels and separators start, from a card's edge.
pub(super) fn text_x(p: &Look) -> f32 {
    inset(p) + lead_w(p) + p.space.s2
}

/// The trailing column's width: a chevron, a check or the clear mark.
pub(super) fn trail_w(p: &Look) -> f32 {
    p.icon.grid * p.type_scale
}

/// The search field's trailing padding that centres its 44 point clear
/// control on the trailing column.
pub(super) fn clear_inset(p: &Look) -> f32 {
    (inset(p) + trail_w(p) / 2.0 - p.space.touch_min / 2.0).max(0.0)
}

/// The space between blocks: Connections, each section, Quiet.
pub(super) fn block_gap(p: &Look) -> f32 {
    p.space.s6
}

/// Grouped cards, the search field and the dock: one step up from the
/// page, so a card reads as a card on the dark drawer as it does on light.
pub(super) fn card_fill(p: &Look) -> Color {
    let layer = &p.roles.layer;
    if p.dark() {
        layer.overlay.paint()
    } else {
        layer.raised.paint()
    }
}

/// `mark` centred in the leading column on a line `line` points tall,
/// so it sits on the first line of the text beside it.
pub(super) fn lead_slot(mark: El, line: f32, p: &Look) -> El {
    el().size(lead_w(p), line).center().child(mark)
}

/// `mark`, if any, centred in the trailing column on a line `line` tall.
pub(super) fn trail_slot(mark: Option<El>, line: f32, p: &Look) -> El {
    el().size(trail_w(p), line)
        .center()
        .children(mark)
        .a11y_hidden()
}
