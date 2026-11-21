//! Marks laid over a pane's card: its number while `display-panes` shows
//! them, and `sync` while the tab shares what is typed.

use phux_protocol::ids::ResourceId;
use rune::{El, el, text};

use super::Paint;
use super::tokens::Tokens;
use super::tokens::Voice;
use crate::desk::Desk;
use crate::ui::Cover;

pub(super) fn marks(desk: &Desk, id: &ResourceId, t: &Tokens) -> Vec<El> {
    let number = desk.pane_numbers.iter().position(|pane| pane == id);
    let number = number.map(|at| {
        let digit = text((at + 1).to_string())
            .font(t.ty.display)
            .color(t.roles.text.text.paint());
        let chip = el()
            .padding(t.space.s4)
            .radius(t.radius.lg)
            .fill(t.roles.layer.raised.paint())
            .border(t.space.hairline, t.roles.border.focus.paint())
            .child(digit);
        el().key(format!("pane-number-{id}"))
            .cover()
            .center()
            .label(format!("Pane {}", at + 1))
            .child(chip)
            .inert()
    });
    let sync = desk.synced(id).then(|| {
        text("sync")
            .key(format!("pane-sync-{id}"))
            .font(t.ty.caption.semibold())
            .color(t.roles.accent.accent_text.paint())
            .pin(Some(t.space.s2), Some(t.space.s1), None, None)
            .label("Typing goes to every pane in this tab")
            .inert()
    });
    number.into_iter().chain(sync).collect()
}
