//! One named destination, with the current computer and its status as
//! context: a mark and a word, never colour alone.
use phux_design::State;
use rune::{El, col, el, row, text};

use super::{Desk, Look, control};
use crate::keys::Shortcut;
use crate::ui::icons;
use crate::ui::tokens::{Paint, Voice};

pub(super) fn view(desk: &Desk, p: &Look) -> El {
    let dim = p.roles.text.dim.paint();
    let state = match desk.link_word() {
        "connected" => State::Done,
        "connecting" => State::Working,
        _ => State::Failed,
    };
    let label = format!(
        "Connections · {} · {}",
        desk.machines.label(),
        desk.link_word()
    );
    let title = text("Connections")
        .key("phone-connections-title")
        .font(p.ty.label.medium())
        .color(p.roles.text.text.paint())
        .wrap_anywhere()
        .a11y_hidden();
    let detail = text(format!("{} · {}", desk.machines.label(), desk.link_word()))
        .key("phone-connections-detail")
        .font(p.ty.caption.host(desk.machines.label()))
        .color(dim)
        .wrap_anywhere()
        .a11y_hidden();
    let mark = el()
        .size(p.icon.size.sm, p.icon.size.sm)
        .radius(p.radius.full)
        .fill(p.status.state(state).tone.ink)
        .a11y_hidden();
    let chevron = rune::icon(&icons::CHEVRON_RIGHT, p.icon.size.md)
        .color(dim)
        .a11y_hidden();
    let destination = control("phone-session-connections", &label, p, |d, tx| {
        d.phone_action(Shortcut::Machines, tx)
    })
    .w_full()
    .gap(p.space.s2)
    .py(p.space.s2);
    if p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale {
        // The destination word gets the full measure at accessibility sizes;
        // the status and disclosure remain together below it, not beside it.
        return destination.flex_wrap().child(title.w_full()).child(
            col()
                .key("phone-connections-detail")
                .w_full()
                .gap(p.space.s1)
                .child(
                    text(desk.machines.label())
                        .font(p.ty.caption.host(desk.machines.label()))
                        .color(dim)
                        .wrap_anywhere(),
                )
                .child(
                    row()
                        .w_full()
                        .items_center()
                        .gap(p.space.s2)
                        // Utility marks keep their standard size; only text
                        // grows. A huge dot must not steal the status word's room.
                        .child(mark.size(
                            phux_design::tokens::ICON.size.sm,
                            phux_design::tokens::ICON.size.sm,
                        ))
                        .child(
                            text(desk.link_word())
                                .font(p.ty.caption)
                                .color(dim)
                                .wrap_anywhere()
                                .grow(1.0)
                                .min_w(0.0),
                        )
                        .child(
                            rune::icon(
                                &icons::CHEVRON_RIGHT,
                                phux_design::tokens::ICON.size.md,
                            )
                            .color(dim),
                        ),
                )
                .a11y_hidden(),
        );
    }
    destination
        .child(mark)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(p.space.s1)
                .child(title)
                .child(detail),
        )
        .child(chevron)
}
