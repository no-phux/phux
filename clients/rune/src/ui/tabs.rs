//! The tab strip, shown once a session has more than one tab, and the
//! rename sheet.

use rune::{Color, El, TextStyle, Transition, col, el, row, text, text_input};

use super::{CAPTION, Ink, LABEL_STRONG};
use crate::activity::Mark;
use crate::desk::{Desk, Renaming, Tab};

pub(super) const STRIP_H: f32 = 32.0;

fn tab(desk: &Desk, tab: &Tab, index: usize, ink: &Ink) -> El {
    let waiting = tab
        .terminals
        .iter()
        .any(|id| desk.activities.mark(id).needs_you());
    let dot = waiting.then(|| {
        el().size(6.0, 6.0)
            .radius(3.0)
            .fill(super::sidebar::mark_color(Mark::Blocked, ink))
    });
    let close = tab.active.then(|| {
        el().key(format!("tab-close-{index}"))
            .size(16.0, 16.0)
            .radius(4.0)
            .center()
            .hover(ink.hover)
            .cursor(rune::Cursor::Pointer)
            .on_press(move |d: &mut Desk| d.close_tab(index))
            .child(text("✕").font(CAPTION).color(ink.dim))
    });
    let hint = (index < 9).then(|| {
        text(format!("{}{}", super::MOD, index + 1))
            .font(CAPTION)
            .color(ink.dim.alpha(0.7))
    });
    row()
        .key(format!("tab-{index}"))
        .h(24.0)
        .max_w(200.0)
        .min_w(0.0)
        .px(9.0)
        .gap(6.0)
        .radius(6.0)
        .items_center()
        .fill(if tab.active {
            ink.selected
        } else {
            Color::TRANSPARENT
        })
        .hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(move |d: &mut Desk| d.select_tab(index))
        .on_double_press(move |d: &mut Desk| d.rename_tab(index))
        .children(dot)
        .child(
            text(desk.tab_label(tab))
                .font(if tab.active {
                    LABEL_STRONG
                } else {
                    super::LABEL
                })
                .color(if tab.active { ink.text } else { ink.dim })
                .truncate(),
        )
        .children(hint)
        .children(close)
}

pub fn strip(desk: &Desk, ink: &Ink) -> Option<El> {
    if desk.tabs.list.len() < 2 {
        return None;
    }
    let mut line = row()
        .key("tabs")
        .w_full()
        .h(STRIP_H)
        .px(6.0)
        .gap(3.0)
        .items_center()
        .fill(ink.sidebar)
        .min_w(0.0);
    for (index, item) in desk.tabs.list.iter().enumerate() {
        line = line.child(tab(desk, item, index, ink));
    }
    Some(
        line.child(
            el().key("tab-new")
                .size(24.0, 24.0)
                .radius(6.0)
                .center()
                .hover(ink.hover)
                .cursor(rune::Cursor::Pointer)
                .on_press(|d: &mut Desk| d.new_tab())
                .child(text("+").font(LABEL_STRONG).color(ink.dim)),
        ),
    )
}

pub fn rename_sheet(desk: &Desk, ink: &Ink) -> Option<El> {
    let title = match desk.tabs.renaming.as_ref()? {
        Renaming::Session(_) => "Rename session",
        Renaming::Tab(_) => "Rename tab",
    };
    let panel = col()
        .key("rename")
        .w(360.0)
        .padding(16.0)
        .gap(10.0)
        .radius(12.0)
        .fill(ink.raised)
        .border(1.0, ink.separator)
        .shadow(18.0, 56.0, Color::BLACK.alpha(0.5))
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .child(text(title).font(LABEL_STRONG).color(ink.text))
        .child(
            row()
                .w_full()
                .px(10.0)
                .py(7.0)
                .radius(7.0)
                .fill(ink.separator)
                .child(
                    text_input(
                        &desk.tabs.rename_field,
                        TextStyle::new(14.0, 400.0).lh(20.0),
                    )
                    .key("rename-field")
                    .single_line()
                    .grow(1.0)
                    .color(ink.text)
                    .on_edit(|d: &mut Desk| &mut d.tabs.rename_field)
                    .on_input_key(|d: &mut Desk, event, tx| d.rename_key(event, tx))
                    .on_submit(|d: &mut Desk, tx| d.submit_rename(tx)),
                ),
        );
    Some(
        el().key("rename-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(Color::BLACK.alpha(0.3))
            .transition(Transition::opacity())
            .modal(true, |d: &mut Desk| d.cancel_rename())
            .child(col().w_full().pt(140.0).items_center().child(panel)),
    )
}
