//! Search, inbox and creation controls around the session navigator.

use phux_design::Tone;
use rune::{El, el, text};
use rune_kit::{Button, ButtonVariant};

use super::super::{Paint, Tokens, icon_press, icons, pressable, tip};
use super::return_to_terminal_on_click;
use crate::desk::Desk;
use crate::keys::Shortcut;

/// The way to the palette: a press target drawn as a filled pill with the
/// sidebar rows' radius, not an outlined field, since nothing is typed
/// here. It names the chord that opens the palette from anywhere.
pub(super) fn search(t: &Tokens) -> El {
    // ⌘K is the chord this field has always taught; ⌘P reaches it too.
    // Plain dim figures, as a menu writes a shortcut, not boxed keycaps.
    let chord = if cfg!(target_os = "macos") {
        "⌘K"
    } else {
        "Ctrl+Shift+K"
    };
    let s = &t.space;
    let dim = t.roles.text.dim.paint();
    pressable("sidebar-search", t)
        .w_full()
        .h(s.row_h)
        .px(s.s2)
        .gap(s.icon_gap)
        .radius(t.radius.md)
        .items_center()
        .fill(t.roles.wash.hover.paint())
        .focusable(true)
        .role("button")
        .label("Jump to…")
        .on_press(|d: &mut Desk| d.toggle_palette())
        .child(
            rune::icon(&icons::SEARCH, t.icon.size.md)
                .color(dim)
                .a11y_hidden(),
        )
        .child(text("Jump to…").font(t.ty.label).color(dim).a11y_hidden())
        .child(el().grow(1.0))
        .child(text(chord).font(t.ty.label).color(dim).a11y_hidden())
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
            return_to_terminal_on_click(tx);
        })
        .build(&t.kit)
        .key(key)
        .grow(1.0)
        .min_w(0.0)
        .label(format!("New {label}"));
    tip(button, format!("New {label}"), action)
}
