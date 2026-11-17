//! Status marks and tone badges (§1.3, §4.1.3 OWN): colour, word and mark
//! together. Not a live region, so rows in long lists stay quiet; kit
//! `Badge` cannot take a tone.

use phux_design::State;
use rune::{El, row, text};

use super::tokens::Tokens;

/// `state`'s vector mark at `size`, named by its word for a screen reader
/// where the word itself is hidden.
pub(crate) fn mark(t: &Tokens, state: State, size: f32) -> El {
    let ink = t.status.state(state);
    let icon = rune::icon(ink.icon, size).color(ink.tone.ink);
    match ink.word {
        Some(word) => icon.label(word),
        None => icon.a11y_hidden(),
    }
}

/// A compact pill: `state`'s mark and `words` (its word when `None`) on
/// the tone's wash and edge. Takes a min-width only, so a translated word
/// never truncates.
pub(crate) fn tone_badge(t: &Tokens, state: State, words: Option<String>) -> El {
    let ink = t.status.state(state);
    let words = words.or_else(|| ink.word.map(str::to_owned));
    let font = if ink.bold {
        rune::TextStyle {
            weight: f32::from(
                u16::try_from(phux_design::tokens::TYPE.weights.semibold).unwrap_or(600),
            ),
            ..t.ty.caption
        }
    } else {
        t.ty.caption
    };
    row()
        .gap(t.space.s1)
        .px(t.space.s1)
        .min_h(t.icon.size.lg)
        .items_center()
        .radius(t.radius.xs)
        .fill(ink.tone.wash)
        .border(t.space.hairline, ink.tone.edge)
        .child(
            rune::icon(ink.icon, t.icon.size.xs)
                .color(ink.tone.ink)
                .a11y_hidden(),
        )
        .children(words.map(|w| text(w).font(font).color(ink.tone.ink)))
}
