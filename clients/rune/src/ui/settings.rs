//! The settings sheet: theme, text, and how the app behaves.

//!
//! Every control here takes the keyboard. Keep `focusable` to sheets: a modal
//! traps focus, while one on the main window would take Tab from the terminal.

use rune::{El, TextStyle, col, el, row, text};

use super::overlay::key_hint;
use super::{CAPTION, Ink, LABEL, Sheet, pressable, updates};
use crate::desk::Desk;
use crate::grid::{MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::theme::{THEMES, Theme, color};

const WIDTH: f32 = 560.0;
const TILE_W: f32 = 120.0;

pub(super) fn section(title: &'static str, ink: &Ink) -> El {
    text(title.to_uppercase())
        .font(TextStyle::new(10.5, 650.0).tracking(0.6))
        .color(ink.dim)
}

fn tile(theme: Option<&'static Theme>, chosen: bool, ink: &Ink) -> El {
    let name = theme.map_or("System", |t| t.name);
    let (back, swatches) = theme.map_or((ink.bg, Vec::new()), |t| {
        (
            color(t.background),
            t.ansi[1..7].iter().map(|c| color(*c)).collect(),
        )
    });
    let mut strip = row().gap(3.0).items_center();
    for swatch in swatches {
        strip = strip.child(el().size(10.0, 10.0).radius(5.0).fill(swatch));
    }
    if theme.is_none() {
        strip = strip.child(text("follows the OS").font(CAPTION).color(ink.dim));
    }
    col()
        .key(format!("theme-{name}"))
        .w(TILE_W)
        .gap(6.0)
        .focusable(true)
        .role("radio")
        .label(name)
        .focus_selected(chosen)
        .cursor(rune::Cursor::Pointer)
        .on_press(move |d: &mut Desk| d.choose_theme(theme))
        .child(
            el().w_full()
                .h(44.0)
                .px(9.0)
                .radius(8.0)
                .fill(back)
                .border(
                    if chosen { 2.0 } else { 1.0 },
                    if chosen { ink.accent } else { ink.separator },
                )
                .items_center()
                .child(strip),
        )
        .child(text(name).font(CAPTION).color(ink.text).truncate())
}

fn themes(desk: &Desk, ink: &Ink) -> El {
    // One Tab stop: arrows rove it, choosing as they go.
    let mut grid = row()
        .key("theme-grid")
        .w_full()
        .gap(10.0)
        .flex_wrap()
        .role("radiogroup")
        .label("Theme")
        .focus_group();
    grid = grid.child(tile(None, desk.settings.theme.is_none(), ink));
    for theme in THEMES {
        grid = grid.child(tile(Some(theme), desk.settings.theme == Some(theme), ink));
    }
    grid
}

fn switch(on: bool, ink: &Ink) -> El {
    let thumb = el()
        .size(14.0, 14.0)
        .radius(7.0)
        .fill(if on { ink.on_accent } else { ink.dim });
    row()
        .w(30.0)
        .h(18.0)
        .px(2.0)
        .radius(9.0)
        .items_center()
        .fill(if on { ink.accent } else { ink.separator })
        .children((on).then(|| el().grow(1.0)))
        .child(thumb)
}

pub(super) fn toggle(
    key: &'static str,
    title: &'static str,
    detail: &'static str,
    on: bool,
    press: fn(&mut Desk),
    ink: &Ink,
) -> El {
    row()
        .key(key)
        .w_full()
        .py(6.0)
        .gap(12.0)
        .items_center()
        .focusable(true)
        .role("switch")
        .label(title)
        .cursor(rune::Cursor::Pointer)
        .on_press(press)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(1.0)
                .child(text(title).font(LABEL).color(ink.text))
                .child(text(detail).font(CAPTION).color(ink.dim).truncate()),
        )
        .child(switch(on, ink))
}

fn stepper(desk: &Desk, ink: &Ink) -> El {
    let step = |label: &'static str, key: &'static str, press: fn(&mut Desk)| {
        pressable(key, ink)
            .size(26.0, 24.0)
            .radius(6.0)
            .center()
            .border(1.0, ink.separator)
            .on_press(press)
            .child(text(label).font(LABEL).color(ink.text))
    };
    row()
        .key("text-size")
        .w_full()
        .py(6.0)
        .gap(8.0)
        .items_center()
        .label("Text size")
        .on_step(|d: &mut Desk, steps, _| d.zoom(Some(d.font_size + steps as f32)))
        .on_step_limit(|d: &mut Desk, max, _| {
            d.zoom(Some(if max { MAX_FONT_SIZE } else { MIN_FONT_SIZE }))
        })
        .child(text("Text size").font(LABEL).color(ink.text))
        .child(el().grow(1.0))
        .child(step("−", "text-smaller", |d| {
            d.zoom(Some(d.font_size - 1.0))
        }))
        .child(
            el().w(44.0).center().child(
                text(format!("{:.0} pt", desk.font_size))
                    .font(CAPTION)
                    .color(ink.text),
            ),
        )
        .child(step("+", "text-bigger", |d| {
            d.zoom(Some(d.font_size + 1.0))
        }))
}

fn behaviour(desk: &Desk, ink: &Ink) -> El {
    let notify_detail = crate::alerts::notifications_detail(
        rune_mac::notify::available(),
        cfg!(target_os = "macos"),
    );
    let option_as_meta = cfg!(target_os = "macos").then(|| {
        toggle(
            "set-option-meta",
            "Option as Meta",
            "Option sends Esc-prefixed keys instead of typing å and ∑.",
            desk.settings.option_as_meta,
            Desk::toggle_option_as_meta,
            ink,
        )
    });
    col()
        .w_full()
        .gap(12.0)
        .child(section("Behaviour", ink))
        .child(toggle(
            "set-notifications",
            "Notifications",
            notify_detail,
            desk.settings.notifications,
            Desk::toggle_notifications,
            ink,
        ))
        .children(option_as_meta)
        .child(toggle(
            "set-copy-select",
            "Copy on select",
            "Finishing a selection puts it on the clipboard.",
            desk.settings.copy_on_select,
            Desk::toggle_copy_on_select,
            ink,
        ))
        .child(toggle(
            "set-confirm-close",
            "Confirm before closing",
            "Ask before closing a terminal or tab that's still running something.",
            desk.settings.confirm_close,
            Desk::toggle_confirm_close,
            ink,
        ))
}

pub fn view(desk: &Desk, ink: &Ink) -> El {
    let body = col()
        .w_full()
        .padding(18.0)
        .gap(12.0)
        .child(section("Theme", ink))
        .child(themes(desk, ink))
        .child(section("Text", ink))
        .child(stepper(desk, ink))
        .child(behaviour(desk, ink))
        .child(updates::settings(desk, ink));
    let version = text(concat!("Phux Rune ", env!("CARGO_PKG_VERSION")))
        .font(CAPTION)
        .color(ink.dim);
    let foot = row()
        .w_full()
        .gap(12.0)
        .items_center()
        .child(key_hint("tab", "next", ink))
        .child(key_hint("←→", "change", ink))
        .child(key_hint("space", "toggle", ink))
        .child(key_hint("esc", "close", ink));
    Sheet {
        key: "settings",
        title: "Settings",
        width: WIDTH,
        top: 72.0,
        // Light enough to judge a theme against the terminals behind it.
        scrim: 0.12,
        close: Desk::close_settings,
    }
    .build(Some(version), body, foot, ink)
}
