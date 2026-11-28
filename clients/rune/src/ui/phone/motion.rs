//! How every phone page comes and goes, each the way its swipe takes it
//! away ([`crate::desk::phone::dismiss`]): the terminal is the root;
//! Sessions is its drawer from the left edge (the navigation layer's own
//! drift and frost); every other page is a sheet that rises from below and
//! lowers again; Settings goes a level in from the trailing edge and comes
//! back out. Pages that share a direction share a spring, and under Reduce
//! Motion Rune lands their travel at once, leaving only a short fade.
use rune::{Edge, Effect, Transition, Vec2};

use crate::ui::tokens::Tokens;

/// A sheet rises by its whole height on the drawer spring and lowers the
/// same way. It arrives whole, never fading, so a glass sheet's blur is
/// never flattened into a fading layer; it leaves fading, as everything
/// does.
pub(in crate::ui) fn sheet(p: &Tokens) -> Transition {
    Transition::move_edge(Edge::Bottom).springs(p.motion.drawer)
}

/// A Settings level: one `deeper` than the list comes in from the trailing
/// edge and goes back out to it; the list it covers drifts toward the
/// leading edge as far as Sessions drifts in from it, and dims, and comes
/// back from there.
pub(in crate::ui) fn level(deeper: bool, p: &Tokens) -> Transition {
    let effect = if deeper {
        Effect {
            offset_fraction: Edge::Trailing.direction(),
            ..Effect::IDENTITY
        }
    } else {
        Effect {
            offset: Vec2::new(-p.space.s16, 0.0),
            opacity: 0.0,
            ..Effect::IDENTITY
        }
    };
    Transition {
        insertion: effect,
        removal: effect,
        ..Transition::identity()
    }
    .springs(p.motion.page)
}

/// What a page's holder (its modal, its scrim) wears: it arrives as
/// nothing, so the page inside it plays its own entrance (Rune plays an
/// entering effect only where its parent's did not), and it leaves as
/// `page` does, taking the page with it.
pub(in crate::ui) fn holding(page: Transition) -> Transition {
    Transition::asymmetric(Transition::identity(), page).springs(page.springs)
}
