//! Complete raw intent is a scrollable review, never an abbreviated toast.
use super::{
    Sheet,
    tokens::{Paint, Tokens},
};
use crate::desk::Desk;
use rune::{El, col, row, text};
use rune_kit::{Button, ButtonVariant};

pub(super) fn view(desk: &Desk, t: &Tokens) -> Option<El> {
    if !desk.raw_relaunch.open() {
        return None;
    }
    let ready = desk.raw_relaunch.can_approve();
    let detail = desk
        .raw_relaunch
        .document
        .as_deref()
        .unwrap_or("Reading the executing host's exact proposal. Nothing has been approved.");
    let body = col().w_full().gap(t.space.s2).px(t.space.sheet_pad).py(t.space.s2)
        .child(text("Untrusted archived executable content. Approval applies only to the exact host, target, revision and operation below; it grants no provider permissions.")
            .font(t.ty.label).color(t.roles.text.text.paint()).wrap())
        .child(text(detail).font(t.ty.mono).color(t.roles.text.text.paint()).wrap().selectable(true));
    let footer = row()
        .gap(t.space.s2)
        .items_center()
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Outline)
                .on_press(Desk::cancel_raw_relaunch)
                .build(&t.kit),
        )
        .children(ready.then(|| {
            let reviewed_document = detail.to_owned();
            Button::new("Approve this exact raw execution")
                .variant(ButtonVariant::Danger)
                .on_press(move |desk: &mut Desk| desk.approve_raw_relaunch(&reviewed_document))
                .build(&t.kit)
        }));
    Some(
        Sheet {
            key: "raw-relaunch-review",
            title: "Review archived raw execution",
            width: 760.0,
            top: t.space.s6,
            close: Desk::cancel_raw_relaunch,
        }
        .build(None, body, Some(footer), t),
    )
}
