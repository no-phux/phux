//! A pairing link waiting in the machines sheet: where it dials, the name
//! it registers, and Pair or Cancel.

use rune::{El, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize};

use super::grown;
use crate::desk::Desk;
use crate::ui::tokens::{Paint, Tokens, Voice};

/// A pairing link waiting to be accepted: where it dials and the name it
/// registers, and whether that replaces a machine already set up.
pub(super) fn offer(desk: &Desk, t: &Tokens) -> Option<El> {
    let s = &t.space;
    let offer = desk.machines.offer.as_ref()?;
    let target = &offer.request.target;
    let mut ask = col().w_full().gap(s.s1).pb(s.s2).child(
        text(format!("Pair with {} as {target}?", offer.peer))
            .font(t.ty.label.semibold())
            .color(t.roles.text.text.paint())
            .wrap(),
    );
    if desk.machines.hosts.iter().any(|host| &host.name == target) {
        ask = ask.child(
            text(format!(
                "This replaces the machine already named {target} and its credentials."
            ))
            .font(t.ty.caption)
            .color(t.status.tone(phux_design::Tone::Failed).ink)
            .wrap(),
        );
    }
    let buttons = row()
        .gap(s.s2)
        .child(
            Button::new("Pair")
                .variant(ButtonVariant::Submit)
                .control_size(ControlSize::Large)
                .on_press(Desk::accept_pairing)
                .build(&t.kit)
                .key("machine-pair-yes")
                .focusable(true)
                .min_h(grown(t.ty.body, t.space.s2)),
        )
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Large)
                .on_press(Desk::decline_pairing)
                .build(&t.kit)
                .key("machine-pair-no")
                .focusable(true)
                .min_h(grown(t.ty.body, t.space.s2)),
        );
    Some(ask.child(buttons))
}
