//! The changes sheet: the checkout's files on the left, the picked file's
//! diff on the right. It shows; it never stages or writes.
//!
//! The diff draws on the `diff.*` roles, never a status wash (D8): kit
//! `DiffView` tints its lines from the status tones and solves its own
//! syntax hues, so the runs stay phux's own on the tokens.

use rune::{Color, Cursor, El, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize};

use super::presentation::{Diff, Line, Row};
use crate::desk::Desk;
use crate::ui::rows::Row as ListRow;
use crate::ui::{Paint, Sheet, Tokens, key_hint};

/// The sheet's geometry: layout, which no token group holds.
struct Extents {
    width: f32,
    list_w: f32,
    /// The files and the diff each scroll inside this.
    body_h: f32,
}

const EXTENTS: Extents = Extents {
    width: 940.0,
    list_w: 290.0,
    body_h: 440.0,
};

fn message(words: impl Into<String>, t: &Tokens) -> El {
    el().w_full().padding(t.space.sheet_pad).child(
        text(words.into())
            .key("changes-message")
            .wrap()
            .w_full()
            .font(t.ty.label)
            .color(t.roles.text.dim.paint()),
    )
}

fn scrolls(key: &'static str) -> El {
    col().key(key).h_full().min_h(0.0).scroll(rune::ScrollSpec {
        stick_bottom: false,
    })
}

/// A file's status letter in the diff's own inks: added and deleted read
/// as the diff does, everything else quiet.
fn code_ink(code: &str, t: &Tokens) -> Color {
    let d = &t.roles.diff;
    match code.trim() {
        "A" | "??" => d.add_text.paint(),
        "D" => d.remove_text.paint(),
        _ => t.roles.text.dim.paint(),
    }
}

/// One file (§4.1.3 OWN row): its status letter, the path in the mono
/// face, its state and counts; the picked file is the keyboard's cursor.
fn file_row(file: &Row, index: usize, picked: bool, t: &Tokens) -> El {
    let name = match &file.from {
        Some(from) => format!("{from} → {}", file.path),
        None => file.path.clone(),
    };
    let code = text(file.code.clone())
        .font(t.ty.mono)
        .color(code_ink(&file.code, t))
        .a11y_hidden();
    let row = ListRow::new(format!("changes-row-{index}"), name.clone())
        .detail(format!("{} {}", file.state, file.counts))
        .leading(code)
        .mono(true)
        .current(picked)
        .build(t);
    t.kit
        .interactive(row)
        .cursor(Cursor::Pointer)
        .label(format!("{name}, {} {}", file.state, file.counts))
        .on_press_tx(move |d: &mut Desk, tx| d.pick_change(index, tx))
}

/// A run of diff lines: added and removed over their diff fills, in the
/// diff's text inks; hunk headers in the accent's text ink.
fn run(kind: Line, lines: &str, t: &Tokens) -> El {
    let d = &t.roles.diff;
    let r = &t.roles;
    let (ink, fill) = match kind {
        Line::Added => (d.add_text, Some(d.add)),
        Line::Removed => (d.remove_text, Some(d.remove)),
        Line::Hunk => (r.accent.accent_text, None),
        Line::Meta => (r.text.dim, None),
        Line::Context => (r.text.text, None),
    };
    let e = el().w_full().px(t.space.s3);
    let e = match fill {
        Some(fill) => e.fill(fill.paint()),
        None => e,
    };
    e.child(
        text(lines.to_owned())
            .selectable(true)
            .wrap_anywhere()
            .w_full()
            .font(t.ty.mono)
            .color(ink.paint()),
    )
}

fn diff_pane(diff: Option<&Result<Diff, String>>, t: &Tokens) -> El {
    let pane = scrolls("changes-diff").grow(1.0).min_w(0.0).py(t.space.s2);
    match diff {
        None => pane.child(message("Reading the diff…", t)),
        Some(Err(words)) => pane.child(message(words.clone(), t)),
        Some(Ok(diff)) => pane
            .children(diff.note.clone().map(|note| message(note, t)))
            .children(diff.runs.iter().map(|(kind, lines)| run(*kind, lines, t))),
    }
}

pub fn view(desk: &Desk, p: &impl AsRef<Tokens>) -> El {
    let t = p.as_ref();
    let s = &t.space;
    let r = &t.roles;
    let changes = &desk.connection.changes;
    let body = match &changes.listing {
        None => message("Reading the checkout…", t),
        Some(Err(words)) => message(words.clone(), t),
        Some(Ok(listing)) => {
            let summary = el().w_full().px(s.sheet_pad).py(s.s2).child(
                text(listing.summary.clone())
                    .key("changes-summary")
                    .font(t.ty.label)
                    .color(r.text.text.paint())
                    .wrap(),
            );
            let files = listing.rows.iter().enumerate();
            let list = scrolls("changes-files")
                .w(EXTENTS.list_w)
                .padding(s.s1)
                .role("list")
                .label("Changed files")
                .children(files.map(|(i, file)| file_row(file, i, i == changes.picked, t)));
            let rest = if listing.rows.is_empty() {
                message("No changes.", t)
            } else {
                row()
                    .w_full()
                    .h(EXTENTS.body_h)
                    .child(list)
                    .child(
                        el().w(s.hairline)
                            .h_full()
                            .fill(r.border.base.paint())
                            .a11y_hidden(),
                    )
                    .child(diff_pane(changes.diff.as_ref(), t))
            };
            col()
                .w_full()
                .child(summary)
                .child(rune::rule(s.hairline, r.border.base.paint()).w_full())
                .child(rest)
        }
    };
    let refresh = Button::new("Refresh")
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .on_press(Desk::refresh_changes)
        .build(&t.kit)
        .key("changes-refresh");
    let foot = row()
        .gap(s.s3)
        .child(key_hint("↑↓", "file", t))
        .child(key_hint("r", "refresh", t))
        .child(key_hint("esc", "close", t));
    let sheet = Sheet {
        key: "changes",
        title: "Changes",
        width: EXTENTS.width,
        top: s.s16,
        scrim: 0.0,
        close: Desk::close_changes,
    };
    sheet.build(Some(refresh), body, foot, t)
}

#[cfg(test)]
mod tests;
