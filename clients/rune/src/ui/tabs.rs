//! The tab strip, shown once a session has more than one tab, and the
//! rename sheet.

use rune::{Color, El, TextStyle, col, el, row, text, text_input};

use super::{CAPTION, Ink, LABEL_STRONG, Sheet, button, icon_press, icons, pressable};
use crate::activity::Mark;
use crate::desk::{Desk, Renaming, Tab};

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
        pressable(format!("tab-close-{index}"), ink)
            .size(16.0, 16.0)
            .radius(4.0)
            .center()
            .on_press(move |d: &mut Desk| d.close_tab(index))
            .child(rune::icon(&icons::CLOSE, 11.0).color(ink.dim))
    });
    let hint = (index < 9).then(|| {
        text(format!("{}{}", super::MOD, index + 1))
            .font(CAPTION)
            .color(ink.dim)
    });
    pressable(format!("tab-{index}"), ink)
        .h(24.0)
        .max_w(200.0)
        .min_w(0.0)
        .px(9.0)
        .gap(6.0)
        .radius(6.0)
        .items_center()
        .fill(if tab.active {
            ink.current
        } else {
            Color::TRANSPARENT
        })
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
        .shrink()
        .gap(3.0)
        .items_center()
        .min_w(0.0);
    for (index, item) in desk.tabs.list.iter().enumerate() {
        line = line.child(tab(desk, item, index, ink));
    }
    Some(
        line.child(icon_press(&icons::PLUS, "tab-new", ink, |d: &mut Desk| {
            d.new_tab();
        })),
    )
}

pub fn rename_sheet(desk: &Desk, ink: &Ink) -> Option<El> {
    let title = match desk.tabs.renaming.as_ref()? {
        Renaming::Session(_) => "Rename session",
        Renaming::Tab(_) => "Rename tab",
    };
    let field = row()
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
        );
    let save = pressable("rename-save", ink)
        .px(12.0)
        .py(6.0)
        .radius(6.0)
        .fill(ink.accent)
        .focusable(true)
        .role("button")
        .label("Save")
        .default_button()
        .on_press_tx(|d: &mut Desk, tx| d.submit_rename(tx))
        .child(text("Save (Return)").font(CAPTION).color(ink.on_accent));
    let cancel = button("Cancel (Esc)", "rename-cancel", ink, Desk::cancel_rename)
        .grow(0.0)
        .px(12.0)
        .focusable(true)
        .role("button")
        .label("Cancel");
    let foot = row()
        .w_full()
        .gap(8.0)
        .child(el().grow(1.0))
        .child(cancel)
        .child(save);
    Some(
        Sheet {
            key: "rename",
            title,
            width: 360.0,
            top: 140.0,
            scrim: 0.3,
            close: Desk::cancel_rename,
        }
        .build(None, col().w_full().padding(16.0).child(field), foot, ink),
    )
}
