//! What works here, under a machine's row: each item's verdict and title,
//! the reason, and the fix to copy or run there.

use crate::ui::tokens::Voice;
use phux_design::State;
use rune::{El, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize};

use super::{GEOMETRY, chip};
use crate::desk::Desk;
use crate::desk::machines::checklist::{Item, Verdict};
use crate::ui::badge::mark;

use crate::ui::tokens::{Paint, Tokens};

/// The verdict as a status mark, colour, mark and word together (§1.3):
/// works is done, off failed, waiting working, unknown the dotted ring.
const fn state(verdict: Verdict) -> (State, &'static str) {
    match verdict {
        Verdict::Ok => (State::Done, "works"),
        Verdict::Off => (State::Failed, "off"),
        Verdict::Waiting => (State::Working, "waiting"),
        Verdict::Unknown => (State::Unknown, "unknown"),
    }
}

fn item(item: &Item, here: &str, copied: bool, phone: bool, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let (state, word) = state(item.verdict);
    let mut card = col()
        .key(format!("check-{}", item.id))
        .w_full()
        .gap(s.s1)
        .px(s.s3)
        .py(s.s2)
        .radius(t.radius.md)
        .fill(r.layer.ground.paint())
        .border(s.hairline, r.border.base.paint())
        .child(
            row()
                .w_full()
                .gap(s.icon_gap)
                .items_center()
                .child(mark(t, state, t.icon.size.md).label(word))
                .child(
                    text(item.title.clone())
                        .font(t.ty.label.semibold())
                        .color(r.text.text.paint())
                        .wrap(),
                ),
        )
        .child(
            text(item.reason.clone())
                .font(t.ty.caption)
                .color(r.text.dim.paint())
                .wrap(),
        );
    if let Some(fix) = &item.fix {
        let (copy, run) = (item.id.clone(), item.id.clone());
        let copy = Button::new(if copied { "Copied" } else { "Copy" })
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press_tx(move |d: &mut Desk, tx| d.copy_fix(&copy, tx))
            .build(&t.kit)
            .key(format!("check-copy-{}", item.id))
            .focusable(true)
            .min_h(crate::ui::settings::grown(t.ty.caption, t.space.icon_gap));
        card = card
            .child(
                el().w_full()
                    .mt(s.s0_5)
                    .px(s.s2)
                    .py(s.s1)
                    .radius(t.radius.xs)
                    .fill(r.layer.sidebar.paint())
                    .child(
                        text(fix.clone())
                            .font(t.ty.caption.machine())
                            .color(r.text.text.paint())
                            .wrap()
                            .selectable(true),
                    ),
            )
            .child(row().gap(s.s1).child(copy).child(chip(
                "Run in a new pane",
                format!("check-run-{}", item.id),
                phone,
                t,
                move |d: &mut Desk| d.run_fix(&run),
            )))
            .child(
                text(format!("Runs on {here}."))
                    .font(t.ty.caption)
                    .color(r.text.dim.paint()),
            );
    }
    card
}

/// `machine`'s checklist, when it is the one out.
pub(super) fn checklist(machine: Option<&str>, desk: &Desk, t: &Tokens) -> Option<El> {
    let shown = desk.machines.checklist.as_ref()?;
    if shown.machine.as_deref() != machine {
        return None;
    }
    let s = &t.space;
    let here = machine.unwrap_or(crate::machines::LOCAL);
    let held = shown.machine.clone();
    let head = row()
        .w_full()
        .items_center()
        .child(
            text("What works here")
                .font(t.ty.label.semibold())
                .color(t.roles.text.text.paint()),
        )
        .child(el().grow(1.0))
        .child(chip(
            "Refresh",
            "check-refresh".to_owned(),
            desk.phone.is_some(),
            t,
            move |d: &mut Desk| {
                d.ask_checklist(held.clone());
            },
        ));
    let body = col()
        .key("checklist")
        .w_full()
        .pl(GEOMETRY.nested)
        .pb(s.s2)
        .gap(s.s1)
        .child(head);
    let note = |words: String| {
        text(words)
            .font(t.ty.caption)
            .color(t.roles.text.dim.paint())
            .wrap()
    };
    Some(match &shown.answer {
        None => body.child(note(format!("Asking {here}…"))),
        Some(Err(why)) => body.child(note(why.clone()).key("check-failure")),
        Some(Ok(items)) if items.is_empty() => body.child(note("Nothing to report.".to_owned())),
        Some(Ok(items)) => items.iter().fold(body, |body, it| {
            let copied = shown.copied.as_deref() == Some(it.id.as_str());
            body.child(item(it, here, copied, desk.phone.is_some(), t))
        }),
    })
}
