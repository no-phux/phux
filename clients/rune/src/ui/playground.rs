//! The playground views (galaxy, reef, pitch, kitchen, pit lane, hive) and
//! the way out every one of them shows. The scenes are illustration; their
//! chrome is `hud` and `labels`, on the window's tokens.

use rune::{Cx, El};

use super::{Tokens, galaxy, hive, kitchen, pitch, pitlane, reef};
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

/// How a playground view draws; how it closes is its `Playground::ALL` row.
type View = fn(&Desk, &Tokens, &mut Cx<Desk>) -> El;

/// The open playground view, if any, with the way out every one of them
/// shows.
pub(super) fn open(desk: &Desk, p: &Tokens, cx: &mut Cx<Desk>) -> Option<El> {
    let scene = desk.playground?;
    let view: View = match scene {
        Playground::Galaxy => galaxy::view,
        Playground::Reef => reef::view,
        Playground::Pitch => pitch::view,
        Playground::Kitchen => kitchen::view,
        Playground::PitLane => pitlane::view,
        Playground::Hive => hive::view,
    };
    let close = scene.scene().close;
    // A child of the view: the view is modal, so anything beside it is inert.
    let right = cx.chrome_insets().trailing;
    Some(view(desk, p, cx).child(hud::esc_chip(p, close, right)))
}
