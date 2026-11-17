//! The popup's box: a scrim, the command line with a close button, and its
//! terminal drawn as any pane is.

use crate::ui::tokens::Voice;
use rune::{El, col, el, row, text};

use super::overlay::{EXTENTS, pop};
use super::tokens::{Paint, Tier};
use super::{PAD, Tokens, icon_press, icons, panes, status};
use crate::desk::Desk;

/// A command's popup: its terminal in a box over the window, with the
/// question bar inside it when the command asks.
pub fn view(desk: &Desk, t: &Tokens) -> Option<El> {
    let p = t;
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
                .font(t.ty.mono.semibold())
                .color(r.text.text.paint())
                .grow(1.0)
                .min_w(0.0),
        )
        .child(
            icon_press(
                &icons::CLOSE,
                "Close",
                "popup-close",
                t,
                Desk::dismiss_popup,
            )
            .help("Close"),
        );
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
        // Escape is the command's: only the button and the chord close.
        super::scrim(col().key("popup-scrim").items_center(), "Popup", None, t)
            .child(margin())
            .child(panel)
            .child(margin()),
    )
}
