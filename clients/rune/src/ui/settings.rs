//! The settings sheet: theme, text, and how the app behaves.

use rune::{Color, El, TextStyle, Transition, col, el, row, text};

use super::{CAPTION, Ink, LABEL, LABEL_STRONG};
use crate::desk::Desk;
use crate::theme::{THEMES, Theme, color};

const WIDTH: f32 = 560.0;
const TILE_W: f32 = 120.0;

fn section(title: &'static str, ink: &Ink) -> El {
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
    let mut grid = row().w_full().gap(10.0).flex_wrap();
    grid = grid.child(tile(None, desk.settings.theme.is_none(), ink));
    for theme in THEMES {
        grid = grid.child(tile(Some(theme), desk.settings.theme == Some(theme), ink));
    }
    grid
}

fn switch(on: bool, ink: &Ink) -> El {
    let thumb = el().size(14.0, 14.0).radius(7.0).fill(Color::WHITE);
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

fn toggle(
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
        el().key(key)
            .size(26.0, 24.0)
            .radius(6.0)
            .center()
            .border(1.0, ink.separator)
            .hover(ink.hover)
            .cursor(rune::Cursor::Pointer)
            .on_press(press)
            .child(text(label).font(LABEL).color(ink.text))
    };
    row()
        .w_full()
        .py(6.0)
        .gap(8.0)
        .items_center()
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

pub fn view(desk: &Desk, ink: &Ink) -> El {
    let notify_detail = if rune_mac::notify::available() {
        "When an agent asks, finishes, or an action is held, and you're elsewhere."
    } else {
        "Needs the installed app; this build can't post."
    };
    let body = col()
        .w_full()
        .padding(18.0)
        .gap(12.0)
        .child(section("Theme", ink))
        .child(themes(desk, ink))
        .child(section("Text", ink))
        .child(stepper(desk, ink))
        .child(section("Behaviour", ink))
        .child(toggle(
            "set-notifications",
            "Notifications",
            notify_detail,
            desk.settings.notifications,
            Desk::toggle_notifications,
            ink,
        ))
        .child(toggle(
            "set-option-meta",
            "Option as Meta",
            "Option sends Esc-prefixed keys instead of typing å and ∑.",
            desk.settings.option_as_meta,
            Desk::toggle_option_as_meta,
            ink,
        ))
        .child(toggle(
            "set-copy-select",
            "Copy on select",
            "Finishing a selection puts it on the clipboard.",
            desk.settings.copy_on_select,
            Desk::toggle_copy_on_select,
            ink,
        ));
    let panel = col()
        .key("settings")
        .w(WIDTH)
        .max_w(WIDTH)
        .radius(14.0)
        .clip_rounded(14.0)
        .fill(ink.raised)
        .border(1.0, ink.separator)
        .shadow(18.0, 56.0, Color::BLACK.alpha(0.5))
        .transition(Transition::scale(0.97).combined(Transition::opacity()))
        .child(
            row()
                .w_full()
                .h(48.0)
                .px(18.0)
                .items_center()
                .justify_between()
                .child(text("Settings").font(LABEL_STRONG).color(ink.text))
                .child(
                    text(concat!("phux rune ", env!("CARGO_PKG_VERSION")))
                        .font(CAPTION)
                        .color(ink.dim),
                ),
        )
        .child(el().w_full().h(1.0).fill(ink.separator))
        .child(
            col()
                .w_full()
                .max_h(560.0)
                .min_h(0.0)
                .scroll(rune::ScrollSpec {
                    stick_bottom: false,
                })
                .child(body),
        );
    el().key("settings-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        // Light enough to judge a theme against the terminals behind it.
        .fill(Color::BLACK.alpha(0.12))
        .transition(Transition::opacity())
        .modal(true, |d: &mut Desk| d.close_settings())
        .child(col().w_full().pt(72.0).items_center().child(panel))
}
