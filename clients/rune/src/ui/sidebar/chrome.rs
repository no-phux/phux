//! Search, inbox and creation controls around the session navigator.

use rune::{El, el, row, text};

use super::super::{Ink, LABEL, icon_press, icons, keycap, pressable, tip};
use super::mark_color;
use crate::activity::Mark;
use crate::desk::Desk;
use crate::keys::Shortcut;

pub(super) fn search(ink: &Ink) -> El {
    // ⌘K is the chord this field has always taught; ⌘P reaches it too.
    let chord: &[&str] = if cfg!(target_os = "macos") {
        &["⌘", "K"]
    } else {
        &["Ctrl", "Shift", "K"]
    };
    pressable("sidebar-search", ink)
        .w_full()
        .h(28.0)
        .px(8.0)
        .gap(7.0)
        .radius(7.0)
        .items_center()
        .fill(ink.text.alpha(0.05))
        .border_gradient(1.0, ink.text.alpha(0.09), ink.text.alpha(0.04))
        .on_press(|d: &mut Desk| d.toggle_palette())
        .child(
            rune::icon(&icons::SEARCH, 14.0)
                .color(ink.dim)
                .a11y_hidden(),
        )
        .child(text("Jump to…").font(LABEL).color(ink.dim))
        .child(el().grow(1.0))
        .child(
            row()
                .gap(2.0)
                .children(chord.iter().map(|key| keycap(*key, ink))),
        )
}

/// The way into the inbox, loud only when something waits.
pub(super) fn inbox_button(waiting: usize, ink: &Ink) -> El {
    icon_press(
        &icons::INBOX,
        "Inbox",
        "inbox-button",
        ink,
        |d: &mut Desk| d.toggle_inbox(),
    )
    .label(format!("Inbox, {waiting} waiting"))
    .help(format!("Inbox, {waiting} waiting"))
    .children((waiting > 0).then(|| {
        el().pin(None, Some(3.0), Some(3.0), None)
            .size(6.0, 6.0)
            .radius(3.0)
            .fill(mark_color(Mark::Blocked, ink))
            .inert()
            .a11y_hidden()
    }))
}

/// Stable-width creation controls; keyboard hints float instead of moving labels.
pub(super) fn foot(
    label: &'static str,
    key: &'static str,
    action: Shortcut,
    ink: &Ink,
    press: fn(&mut Desk),
) -> El {
    tip(
        pressable(key, ink)
            .grow(1.0)
            .min_w(0.0)
            .h(30.0)
            .px(6.0)
            .gap(5.0)
            .radius(6.0)
            .items_center()
            .label(format!("New {label}"))
            .on_press(press)
            .child(rune::icon(&icons::PLUS, 12.0).color(ink.dim).a11y_hidden())
            .child(text(label).font(LABEL).color(ink.text)),
        format!("New {label}"),
        action,
    )
}
