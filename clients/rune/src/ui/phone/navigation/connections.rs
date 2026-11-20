//! One named destination, with the current computer and its status as context.
use rune::{El, col, text};

use super::{Desk, Look, control};
use crate::keys::Shortcut;
use crate::ui::icons;
use crate::ui::tokens::{Paint, Voice};

pub(super) fn view(desk: &Desk, p: &Look) -> El {
    let dim = p.roles.text.dim.paint();
    let label = format!(
        "Connections · {} · {}",
        desk.machines.label(),
        desk.link_word()
    );
    control("phone-session-connections", &label, p, |d, tx| {
        d.phone_action(Shortcut::Machines, tx)
    })
    .w_full()
    .gap(p.space.s2)
    .py(p.space.s2)
    .fill(p.roles.layer.raised.paint())
    .child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(p.space.s1)
            .child(
                text("Connections")
                    .key("phone-connections-title")
                    .font(p.ty.label.medium())
                    .color(p.roles.text.text.paint())
                    .wrap()
                    .a11y_hidden(),
            )
            .child(
                text(format!("{} · {}", desk.machines.label(), desk.link_word()))
                    .key("phone-connections-detail")
                    .font(p.ty.caption.host(desk.machines.label()))
                    .color(dim)
                    .wrap()
                    .a11y_hidden(),
            ),
    )
    .child(
        rune::icon(&icons::CHEVRON_RIGHT, p.icon.size.md)
            .color(dim)
            .a11y_hidden(),
    )
}
