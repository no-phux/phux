//! The changes sheet: the checkout's files on the left, the picked file's
//! diff on the right. It shows; it never stages or writes.

use rune::{Color, El, TextStyle, col, el, row, text};

use super::overlay::key_hint;
use super::{CAPTION, Ink, LABEL, Sheet, pressable};
use crate::changes::{Diff, Line, Row};
use crate::desk::Desk;

const LIST_W: f32 = 290.0;
/// The files and the diff each scroll inside this.
const BODY_H: f32 = 440.0;
const MONO: TextStyle = TextStyle::new(12.0, 400.0).mono();

fn message(words: impl Into<String>, p: &Ink) -> El {
    el().w_full().padding(18.0).child(
        text(words.into())
            .key("changes-message")
            .wrap()
            .w_full()
            .font(LABEL)
            .color(p.dim),
    )
}

fn scrolls(key: &'static str) -> El {
    col().key(key).h_full().min_h(0.0).scroll(rune::ScrollSpec {
        stick_bottom: false,
    })
}

fn file_row(file: &Row, index: usize, picked: bool, p: &Ink) -> El {
    let name = match &file.from {
        Some(from) => format!("{from} → {}", file.path),
        None => file.path.clone(),
    };
    pressable(format!("changes-row-{index}"), p)
        .w_full()
        .px(10.0)
        .py(5.0)
        .gap(8.0)
        .radius(6.0)
        .items_center()
        .fill(if picked {
            p.current
        } else {
            Color::TRANSPARENT
        })
        .label(format!("{name}, {} {}", file.state, file.counts))
        .on_press_tx(move |d: &mut Desk, tx| d.pick_change(index, tx))
        .child(text(file.code.clone()).font(MONO).color(p.accent))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .child(text(name).font(LABEL).color(p.text).wrap_anywhere())
                .child(
                    text(format!("{} {}", file.state, file.counts))
                        .font(CAPTION)
                        .color(p.dim),
                ),
        )
}

/// A run of diff lines: added on the theme's good ink, removed on its
/// danger ink, each over a wash of the same.
fn run(kind: Line, lines: &str, p: &Ink) -> El {
    let (ink, wash) = match kind {
        Line::Added => (p.marks.good, p.marks.good.alpha(0.12)),
        Line::Removed => (p.danger, p.danger.alpha(0.12)),
        Line::Hunk => (p.accent, Color::TRANSPARENT),
        Line::Meta => (p.dim, Color::TRANSPARENT),
        Line::Context => (p.text, Color::TRANSPARENT),
    };
    el().w_full().px(12.0).fill(wash).child(
        text(lines.to_owned())
            .selectable(true)
            .wrap_anywhere()
            .w_full()
            .font(MONO)
            .color(ink),
    )
}

fn diff_pane(diff: Option<&Result<Diff, String>>, p: &Ink) -> El {
    let pane = scrolls("changes-diff").grow(1.0).min_w(0.0).py(8.0);
    match diff {
        None => pane.child(message("Reading the diff…", p)),
        Some(Err(words)) => pane.child(message(words.clone(), p)),
        Some(Ok(diff)) => pane
            .children(diff.note.clone().map(|note| message(note, p)))
            .children(diff.runs.iter().map(|(kind, lines)| run(*kind, lines, p))),
    }
}

pub fn view(desk: &Desk, p: &Ink) -> El {
    let changes = &desk.changes;
    let body = match &changes.listing {
        None => message("Reading the checkout…", p),
        Some(Err(words)) => message(words.clone(), p),
        Some(Ok(listing)) => {
            let summary = el().w_full().px(18.0).py(10.0).child(
                text(listing.summary.clone())
                    .key("changes-summary")
                    .font(LABEL)
                    .color(p.text)
                    .wrap(),
            );
            let files = listing.rows.iter().enumerate();
            let list = scrolls("changes-files")
                .w(LIST_W)
                .padding(6.0)
                .gap(1.0)
                .children(files.map(|(i, file)| file_row(file, i, i == changes.picked, p)));
            let rest = if listing.rows.is_empty() {
                message("No changes.", p)
            } else {
                row()
                    .w_full()
                    .h(BODY_H)
                    .child(list)
                    .child(el().w(1.0).h_full().fill(p.separator))
                    .child(diff_pane(changes.diff.as_ref(), p))
            };
            col()
                .w_full()
                .child(summary)
                .child(rune::rule(1.0, p.separator).w_full())
                .child(rest)
        }
    };
    let refresh = pressable("changes-refresh", p)
        .px(8.0)
        .py(3.0)
        .radius(6.0)
        .border(1.0, p.separator)
        .label("Refresh")
        .on_press(Desk::refresh_changes)
        .child(text("Refresh").font(CAPTION).color(p.text));
    let foot = row()
        .gap(14.0)
        .child(key_hint("↑↓", "file", p))
        .child(key_hint("r", "refresh", p))
        .child(key_hint("esc", "close", p));
    let sheet = Sheet {
        key: "changes",
        title: "Changes",
        width: 940.0,
        top: 64.0,
        scrim: 0.3,
        close: Desk::close_changes,
    };
    sheet.build(Some(refresh), body, foot, p)
}
