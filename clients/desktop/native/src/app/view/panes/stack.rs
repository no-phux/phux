//! A zoomed pane's deck: the panes it hides peek out above it as the edges
//! of cards stacked behind, and a click on them restores the split.

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, IntoElement, ParentElement, SharedString, Styled,
    Window, div, ease_out_quint, px,
};
use gpui_kit::component::tooltip::Tooltip;

use crate::workspace::layout::Placement;

use super::super::super::desk::Desk;
use super::super::super::skin::Look;
use super::card::pane_view;

/// How far each sheet behind peeks out above the one in front.
const SHEET_PEEK: f32 = 6.;
/// How much narrower each sheet behind is, on either side.
const SHEET_STEP: f32 = 14.;
/// The most sheets drawn, however many panes hide.
const MAX_SHEETS: usize = 2;

/// How many sheets show for `behind` hidden panes.
pub(super) fn sheets(behind: usize) -> usize {
    behind.min(MAX_SHEETS)
}

/// The zoomed `placement` in front of `behind` hidden panes. The card gives
/// up the sheets' peek at its top, so the hidden panes read as stacked behind
/// it rather than gone.
pub(super) fn zoom_stack(
    desk: &mut Desk,
    placement: &Placement,
    behind: usize,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let card = pane_view(desk, placement, window, cx);
    let count = sheets(behind);
    if count == 0 {
        return card;
    }
    let tip = match (behind, desk.shown_chord("zoom")) {
        (1, Some(chord)) => format!("1 pane behind · Restore  {chord}"),
        (1, None) => "1 pane behind · Restore".to_owned(),
        (_, Some(chord)) => format!("{behind} panes behind · Restore  {chord}"),
        (_, None) => format!("{behind} panes behind · Restore"),
    };
    let moves = !cx.reduce_motion() && desk.prefs.animations;
    let radius = look.skin.shape.radius.min(SHEET_PEEK);
    let (card_fill, ink) = (look.card, look.ink);
    let deck = div()
        .id("zoom-stack")
        .debug_selector(|| "zoom-stack".into())
        .group("zoom-stack")
        .flex_shrink_0()
        .relative()
        .w_full()
        .h(px(SHEET_PEEK * count as f32))
        .cursor_pointer()
        // Farthest first: each nearer sheet is wider and sits lower.
        .children((0..count).map(|row| {
            let back = (count - row) as f32;
            div()
                .absolute()
                .top(px(row as f32 * SHEET_PEEK))
                .h(px(SHEET_PEEK))
                .left(px(back * SHEET_STEP))
                .right(px(back * SHEET_STEP))
                .rounded_t(px(radius))
                .bg(card_fill)
                .child(
                    // Lit from above, fainter the farther back it sits.
                    div()
                        .size_full()
                        .rounded_t(px(radius))
                        .bg(ink.opacity(0.11 / back))
                        .border_t_1()
                        .border_l_1()
                        .border_r_1()
                        .border_color(ink.opacity(0.2 / back))
                        .group_hover("zoom-stack", move |style| {
                            style
                                .bg(ink.opacity(0.2 / back))
                                .border_color(ink.opacity(0.32 / back))
                        }),
                )
        }))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(|desk, _, window, cx| {
            cx.stop_propagation();
            desk.execute("zoom", window, cx);
        }));
    let deck = if moves {
        deck.with_animation(
            SharedString::from(format!("zoom-stack-{}", placement.id)),
            Animation::new(look.skin.motion.pane_enter()).with_easing(ease_out_quint()),
            |deck, delta| deck.opacity(delta),
        )
        .into_any_element()
    } else {
        deck.into_any_element()
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(deck)
        .child(div().flex_1().min_h_0().child(card))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deck_shows_at_most_two_sheets() {
        assert_eq!(sheets(0), 0);
        assert_eq!(sheets(1), 1);
        assert_eq!(sheets(2), 2);
        assert_eq!(sheets(7), 2);
    }
}
