//! The popup's box: a scrim, the command line with a close button, and its
//! terminal drawn as any pane is.

use rune::{El, Transition, col, el, row, text};

use super::overlay::{EXTENTS, icon_button, pop, strong};
use super::tokens::{Paint, Tier};
use super::{Ink, PAD, icons, panes, status};
use crate::desk::Desk;
use crate::ui::Cover;

/// `p` stays the lane-period `Ink`: the pane and the question bar inside
/// the box still take it.
pub fn view(desk: &Desk, p: &Ink) -> Option<El> {
    let t = &*p.tokens;
    let s = &t.space;
    let r = &t.roles;
    let command = desk.connection.popup.command()?;
    let body = match desk.connection.popup.terminal() {
        Some(id) => panes::pane(desk, id, false, p),
        // Laid out first, so the command starts at the size it will have.
        None => el()
            .key("popup-opening")
            .w_full()
            .h_full()
            .center()
            .on_layout(|d: &mut Desk, [_, _, w, h]| {
                d.popup_laid_out([w - 2.0 * PAD, h - 2.0 * PAD]);
            })
            .child(
                text("Opening…")
                    .font(t.ty.caption)
                    .color(r.text.dim.paint()),
            ),
    };
    let header = row()
        .w_full()
        .min_h(s.row_h + s.s1)
        .pl(s.s3)
        .pr(s.s1)
        .gap(s.s2)
        .items_center()
        .child(
            text(command.to_owned())
                .font(strong(t.ty.mono))
                .color(r.text.text.paint())
                .grow(1.0)
                .min_w(0.0),
        )
        .child(icon_button(
            &icons::CLOSE,
            "Close",
            "popup-close",
            t,
            Desk::dismiss_popup,
        ));
    let share = EXTENTS.popup_share;
    let panel = col()
        .key("popup")
        .w_frac(share)
        .grow(share)
        .min_h(0.0)
        .radius(t.radius.lg)
        .fill(r.layer.raised.paint())
        .border(s.hairline, r.border.base.paint())
        .transition(pop(t))
        .child(header)
        .child(
            el().grow(1.0)
                .w_full()
                .min_h(0.0)
                .child(body)
                .children(status::question_bar(desk, p)),
        );
    let panel = t.elevate(panel, Tier::Overlay);
    let margin = || el().grow((1.0 - share) / 2.0);
    Some(
        col()
            .key("popup-scrim")
            .cover()
            .items_center()
            .fill(r.layer.scrim.paint())
            .transition(Transition::opacity())
            // Escape is the command's: only the button and the chord close.
            .modal(false, |_: &mut Desk| {})
            .label("Popup")
            .child(margin())
            .child(panel)
            .child(margin()),
    )
}
