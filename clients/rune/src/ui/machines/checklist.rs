//! What works here, under a machine's row: each item's verdict and title,
//! the reason, and the fix to copy or run there.

use rune::{El, TextStyle, col, el, row, text};

use super::chip;
use crate::desk::Desk;
use crate::desk::machines::checklist::{Item, Verdict};
use crate::ui::{CAPTION, Ink, LABEL, LABEL_STRONG, pressable};

const MONO: TextStyle = TextStyle::new(11.5, 400.0).mono();

/// The verdict's mark, its colour, and what a screen reader says.
fn mark(verdict: Verdict, ink: &Ink) -> (&'static str, rune::Color, &'static str) {
    match verdict {
        Verdict::Ok => ("✓", ink.marks.good, "works"),
        Verdict::Off => ("✕", ink.danger, "off"),
        Verdict::Waiting => ("…", ink.marks.working, "waiting"),
        Verdict::Unknown => ("?", ink.dim, "unknown"),
    }
}

fn item(item: &Item, here: &str, copied: bool, ink: &Ink) -> El {
    let (glyph, color, word) = mark(item.verdict, ink);
    let mut card = col()
        .key(format!("check-{}", item.id))
        .w_full()
        .gap(3.0)
        .px(10.0)
        .py(8.0)
        .radius(7.0)
        .fill(ink.current)
        .child(
            row()
                .w_full()
                .gap(7.0)
                .items_center()
                .child(text(glyph).font(LABEL_STRONG).color(color).label(word))
                .child(text(item.title.clone()).font(LABEL).color(ink.text).wrap()),
        )
        .child(
            text(item.reason.clone())
                .font(CAPTION)
                .color(ink.dim)
                .wrap(),
        );
    if let Some(fix) = &item.fix {
        let (copy, run) = (item.id.clone(), item.id.clone());
        let copy = pressable(format!("check-copy-{}", item.id), ink)
            .px(6.0)
            .radius(5.0)
            .on_press_tx(move |d: &mut Desk, tx| d.copy_fix(&copy, tx))
            .child(
                text(if copied { "Copied" } else { "Copy" })
                    .font(CAPTION)
                    .color(ink.accent),
            );
        card = card
            .child(
                el().w_full()
                    .mt(2.0)
                    .px(8.0)
                    .py(6.0)
                    .radius(5.0)
                    .fill(ink.bg)
                    .child(
                        text(fix.clone())
                            .font(MONO)
                            .color(ink.text)
                            .wrap()
                            .selectable(true),
                    ),
            )
            .child(row().gap(6.0).child(copy).child(chip(
                "Run in a new pane",
                format!("check-run-{}", item.id),
                ink,
                move |d: &mut Desk| d.run_fix(&run),
            )))
            .child(
                text(format!("Runs on {here}."))
                    .font(CAPTION)
                    .color(ink.dim),
            );
    }
    card
}

/// `machine`'s checklist, when it is the one out.
pub(super) fn checklist(machine: Option<&str>, desk: &Desk, ink: &Ink) -> Option<El> {
    let shown = desk.machines.checklist.as_ref()?;
    if shown.machine.as_deref() != machine {
        return None;
    }
    let here = machine.unwrap_or(crate::machines::LOCAL);
    let held = shown.machine.clone();
    let head = row()
        .w_full()
        .items_center()
        .child(text("What works here").font(LABEL_STRONG).color(ink.text))
        .child(el().grow(1.0))
        .child(chip(
            "Refresh",
            "check-refresh".to_owned(),
            ink,
            move |d: &mut Desk| {
                d.ask_checklist(held.clone());
            },
        ));
    let body = col()
        .key("checklist")
        .w_full()
        .pl(40.0)
        .pb(6.0)
        .gap(5.0)
        .child(head);
    let note = |words: String| text(words).font(CAPTION).color(ink.dim).wrap();
    Some(match &shown.answer {
        None => body.child(note(format!("Asking {here}…"))),
        Some(Err(why)) => body.child(note(why.clone()).key("check-failure")),
        Some(Ok(items)) if items.is_empty() => body.child(note("Nothing to report.".to_owned())),
        Some(Ok(items)) => items.iter().fold(body, |body, it| {
            let copied = shown.copied.as_deref() == Some(it.id.as_str());
            body.child(item(it, here, copied, ink))
        }),
    })
}
