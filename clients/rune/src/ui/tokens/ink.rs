//! The lane-period `Ink`, every field filled from the tokens. Views read
//! `Tokens` once they convert; `Ink` and this go in Lane A's last commit.

use std::rc::Rc;

use super::{Paint, Tier, Tokens};
use crate::theme::Roles as Legacy;
use crate::ui::Ink;

pub(super) fn of(t: Rc<Tokens>) -> Ink {
    let r = &t.roles;
    let legacy = Legacy::of(r);
    let lut = phux_design::Params::house().srgb_lut;
    let on_accent_text = phux_design::solve::on_fill(r.accent.accent_text.rgb(), lut);
    Ink {
        bg: r.layer.ground.paint(),
        raised: r.layer.raised.paint(),
        sidebar: r.layer.sidebar.paint(),
        text: r.text.text.paint(),
        dim: r.text.dim.paint(),
        separator: r.border.base.paint(),
        selected: r.wash.selected.paint(),
        current: r.wash.current.paint(),
        hover: r.wash.hover.paint(),
        // Views drew `accent` as ink as often as as a fill, so the legacy
        // field is the legible accent, with an ink that reads on it, as the
        // old roles had it. Converted views pick `accent` or `accent_text`.
        accent: r.accent.accent_text.paint(),
        host: t.host,
        on_accent: on_accent_text.paint(),
        danger: r.status.failed.paint(),
        edge: legacy.edge,
        marks: legacy.marks,
        selection: legacy.selection,
        shade: t.tiers.drop_color(Tier::Floating),
        tokens: t,
    }
}
