//! Search, inbox and creation controls around the session navigator.

use phux_design::Tone;
use rune::{El, el};
use rune_kit::{Button, ButtonVariant};

use super::super::{Tokens, icon_press, icons, tip};
use crate::desk::Desk;
use crate::keys::Shortcut;

/// The way to the palette: an icon beside the inbox, its chord waiting in
/// the tooltip, so the list starts right under the window's top edge.
pub(super) fn search(t: &Tokens) -> El {
    let button = icon_press(
        &icons::SEARCH,
        "Jump to…",
        "sidebar-search",
        t,
        |d: &mut Desk| {
            d.toggle_palette();
        },
    );
    tip(button, "Jump to…", Shortcut::Palette)
}

/// The way into the inbox, loud only when something waits: a dot in the
/// needs-you tone, its glow the one coloured shadow it may cast.
pub(super) fn inbox_button(waiting: usize, t: &Tokens) -> El {
    let needs = t.status.tone(Tone::NeedsYou);
    let words = phux_design::plural("count.needs_you", u64::try_from(waiting).unwrap_or(0))
        .unwrap_or_else(|| waiting.to_string());
    icon_press(&icons::INBOX, "Inbox", "inbox-button", t, |d: &mut Desk| {
        d.toggle_inbox()
    })
    .label(format!("Inbox, {waiting} waiting"))
    .help(format!("Inbox, {words}"))
    .children((waiting > 0).then(|| {
        let size = super::dot_size(t);
        let dot = el()
            .pin(None, Some(t.space.s1), Some(t.space.s1), None)
            .size(size, size)
            .radius(t.radius.full)
            .fill(needs.ink)
            .inert()
            .a11y_hidden();
        t.elevate_glowing(dot, super::super::tokens::Tier::Flat, needs.glow)
    }))
}

/// Stable-width creation controls (kit ghost buttons); keyboard hints wait
/// in the tooltip instead of moving labels. A click hands the keyboard back
/// to the terminal, or the next Space would create another.
pub(super) fn foot(
    label: &'static str,
    key: &'static str,
    action: Shortcut,
    t: &Tokens,
    press: fn(&mut Desk),
) -> El {
    let button = Button::new(label)
        .variant(ButtonVariant::Ghost)
        .icon(&icons::PLUS)
        .on_press_tx(move |d: &mut Desk, tx| {
            press(d);
            super::return_to_terminal_on_click(tx);
        })
        .build(&t.kit)
        .key(key)
        .grow(1.0)
        .min_w(0.0)
        .label(format!("New {label}"));
    tip(button, format!("New {label}"), action)
}
