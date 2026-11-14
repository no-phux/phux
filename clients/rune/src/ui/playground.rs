//! The playground views (galaxy, reef, pitch, kitchen, pit lane, hive) and
//! the way out every one of them shows.

use rune::{Cx, El, text};

use super::{CAPTION, FROST, Ink, galaxy, hive, keycap, kitchen, pitch, pitlane, pressable, reef};
use crate::desk::Desk;

/// A playground view: whether it is open, how it draws, how it closes.
type View = (bool, fn(&Desk, &Ink, &mut Cx<Desk>) -> El, fn(&mut Desk));

/// The open playground view, if any, with the way out every one of them
/// shows. A new view is one more row here.
pub(super) fn open(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> Option<El> {
    let views: [View; 6] = [
        (desk.galaxy.open, galaxy::view, Desk::close_galaxy),
        (desk.reef.open, reef::view, Desk::close_reef),
        (desk.pitch.open, pitch::view, Desk::close_pitch),
        (desk.kitchen.open, kitchen::view, Desk::close_kitchen),
        (desk.pitlane.open, pitlane::view, Desk::close_pitlane),
        (desk.hive.open, hive::view, Desk::close_hive),
    ];
    let (_, view, close) = views.into_iter().find(|v| v.0)?;
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
