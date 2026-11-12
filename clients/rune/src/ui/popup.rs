//! The popup's box: a scrim, the command line with a close button, and its
//! terminal drawn as any pane is.

use rune::{Color, El, Transition, col, el, row, text};

use super::{CAPTION, Ink, LABEL_STRONG, PAD, icons, overlay, panes, status};
use crate::desk::Desk;

/// The share of the window the box takes, each way.
const SHARE: f32 = 0.8;

pub fn view(desk: &Desk, p: &Ink) -> Option<El> {
    let command = desk.popup.command()?;
    let body = match desk.popup.terminal() {
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
            .child(text("Opening…").font(CAPTION).color(p.dim)),
    };
    let header = row()
        .w_full()
        .h(34.0)
        .pl(12.0)
        .pr(6.0)
        .gap(8.0)
        .items_center()
        .child(
            text(command.to_owned())
                .font(LABEL_STRONG)
                .color(p.text)
                .grow(1.0)
                .min_w(0.0),
        )
        .child(overlay::icon_button(
            &icons::CLOSE,
            "Close",
            "popup-close",
            p,
            Desk::dismiss_popup,
        ));
    let panel = col()
        .key("popup")
        .w_frac(SHARE)
        .grow(SHARE)
        .min_h(0.0)
        .radius(12.0)
        .fill(p.raised)
        .border(1.0, p.separator)
        .shadow(18.0, 56.0, Color::BLACK.alpha(0.5))
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .child(header)
        .child(
            el().grow(1.0)
                .w_full()
                .min_h(0.0)
                .child(body)
                .children(status::question_bar(desk, p)),
        );
    let margin = || el().grow((1.0 - SHARE) / 2.0);
    Some(
        col()
            .key("popup-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .items_center()
            .fill(Color::BLACK.alpha(0.4))
            .transition(Transition::opacity())
            // Escape is the command's: only the button and the chord close.
            .modal(false, |_: &mut Desk| {})
            .label("Popup")
            .child(margin())
            .child(panel)
            .child(margin()),
    )
}
