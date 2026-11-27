//! One named destination, with the current computer and its status as
//! context: a mark and a word, never colour alone. A one-row grouped card
//! on the rows' grid, so it leads the list as one of its sections.
use phux_design::State;
use rune::{El, col, el, row, text};

use super::{Desk, Look, card, control, grid};
use crate::keys::Shortcut;
use crate::ui::icons;
use crate::ui::tokens::Paint;

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
        .font(p.ty.body)
        .color(p.roles.text.text.paint())
        .wrap_anywhere()
        .a11y_hidden();
    let detail = text(format!("{} · {}", desk.machines.label(), desk.link_word()))
        .key("phone-connections-detail")
        .font(p.ty.caption)
        .color(dim)
        .wrap_anywhere()
        .a11y_hidden();
    let mark = el()
        .size(p.icon.size.sm, p.icon.size.sm)
        .radius(p.radius.full)
        .fill(p.status.state(state).tone.ink)
        .a11y_hidden();
    let destination = control("phone-session-connections", &label, p, |d, tx| {
        d.phone_action(Shortcut::Machines, tx)
    })
    .w_full()
    .gap(p.space.s2)
    .px(grid::inset(p))
    .py(p.space.s2 + p.space.s0_5);
    if p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale {
        // The destination word gets the full measure at accessibility sizes;
        // the status and disclosure remain together below it, not beside it.
        return card(
            [destination.flex_wrap().child(title.w_full()).child(
                col()
                    .key("phone-connections-detail")
                    .w_full()
                    .gap(p.space.s1)
                    .child(
                        text(desk.machines.label())
                            .font(p.ty.caption)
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
                        .child(super::section::chevron(true, p)),
                    )
                    .a11y_hidden(),
            )],
            p,
        );
    }
    // The computer's mark in the rows' leading column, its link state as
    // the badge a pane's agent state wears.
    let tone = p.status.state(state);
    let machine = el()
        .size(grid::glyph(p), grid::glyph(p))
        .center()
        .child(rune::icon(&icons::MACHINES, grid::glyph(p)).color(dim))
        .child(crate::ui::tile::state_badge(
            tone.icon,
            tone.tone.ink,
            grid::card_fill(p),
            grid::glyph(p),
            p,
        ))
        .a11y_hidden();
    let line = p.ty.body.line_px();
    card(
        [destination
            .items_start()
            .child(grid::lead_slot(machine, line, p).key("phone-connections-mark"))
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(p.space.s0_5)
                    .child(title)
                    .child(detail),
            )
            .child(super::section::chevron(true, p).self_center())],
        p,
    )
}
