//! The playground views (galaxy, reef, pitch, kitchen, pit lane, hive) and
//! the way out every one of them shows.

use rune::{Cx, El, text};

use super::{CAPTION, FROST, Ink, galaxy, hive, keycap, kitchen, pitch, pitlane, pressable, reef};
use crate::desk::Desk;
use crate::desk::surfaces::Playground;

pub(super) mod edges;
pub(super) mod labels;
pub(super) mod pen;

/// How a playground view draws; how it closes is its `Playground::ALL` row.
type View = fn(&Desk, &Ink, &mut Cx<Desk>) -> El;

/// The open playground view, if any, with the way out every one of them
/// shows.
pub(super) fn open(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> Option<El> {
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
    Some(view(desk, p, cx).child(esc_chip(p, close, right)))
}

/// A frosted key cap, top right in the titlebar band clear of every view's
/// own chrome: `esc` closes this. Escape lets go of a pick first; a press
/// here always closes.
fn esc_chip(p: &Ink, close: fn(&mut Desk), right: f32) -> El {
    pressable("playground-esc", p)
        .pin(None, Some(9.0), Some(16.0 + right), None)
        .h(24.0)
        .pl(4.0)
        .pr(9.0)
        .gap(6.0)
        .items_center()
        .radius(8.0)
        .continuous_corners()
        .backdrop_blur(FROST)
        .fill(p.raised.alpha(0.72))
        .border(1.0, p.text.alpha(0.14))
        .shadow(4.0, 14.0, p.shade)
        .hover(p.text.alpha(0.08))
        .label("Close, Escape")
        .on_press(close)
        .child(keycap("esc", p))
        .child(text("close").font(CAPTION).color(p.dim).a11y_hidden())
}
