//! The playground views (galaxy, reef, pitch, kitchen, pit lane, hive) and
//! the way out every one of them shows. The scenes are illustration; their
//! chrome is `hud` and `labels`, on the window's tokens.

use rune::{Cx, El};

use super::{Ink, galaxy, hive, kitchen, pitch, pitlane, reef};
use crate::desk::Desk;
use crate::desk::surfaces::Playground;

#[allow(clippy::disallowed_methods)] // illustration: the scenes' vignette
pub(super) mod edges;
pub(super) mod hud;
pub(super) mod labels;
#[allow(clippy::disallowed_methods)] // illustration: the scenes' painter
pub(super) mod pen;
#[cfg(test)]
mod tests;

/// A playground view: whether it is open, how it draws, how it closes.
type View = (fn(&Desk, &Ink, &mut Cx<Desk>) -> El, fn(&mut Desk));

/// The open playground view, if any, with the way out every one of them
/// shows. A new view is one more row here.
pub(super) fn open(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> Option<El> {
    let (view, close): View = match desk.playground? {
        Playground::Galaxy => (galaxy::view, Desk::close_galaxy),
        Playground::Reef => (reef::view, Desk::close_reef),
        Playground::Pitch => (pitch::view, Desk::close_pitch),
        Playground::Kitchen => (kitchen::view, Desk::close_kitchen),
        Playground::PitLane => (pitlane::view, Desk::close_pitlane),
        Playground::Hive => (hive::view, Desk::close_hive),
    };
    // A child of the view: the view is modal, so anything beside it is inert.
    let right = cx.chrome_insets().trailing;
    Some(view(desk, p, cx).child(hud::esc_chip(&p.tokens, close, right)))
}
