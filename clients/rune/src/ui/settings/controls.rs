//! The few reusable settings controls. Shared switches also serve Updates.

use rune::{El, Family, TextStyle, col, el, row, text};

use super::super::{CAPTION, Ink, LABEL, pressable};
use crate::desk::Desk;
use crate::grid::{MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::theme::color;

pub(in crate::ui) fn section(title: &'static str, ink: &Ink) -> El {
    text(title)
        .font(TextStyle::new(12.0, 600.0))
        .color(ink.dim)
        .role("heading")
        .level(2)
}

pub(super) fn card(rows: Vec<El>, ink: &Ink) -> El {
    let mut card = col()
        .w_full()
        .radius(16.0)
        .clip_rounded(16.0)
        .fill(super::surface(ink))
        .border(1.0, ink.separator);
    for (index, item) in rows.into_iter().enumerate() {
        if index > 0 {
            card = card.child(
                el().h(1.0)
                    .w_full()
                    .px(16.0)
                    .child(el().w_full().h(1.0).fill(ink.separator))
                    .a11y_hidden(),
            );
        }
        card = card.child(item);
    }
    card
}

fn switch(on: bool, ink: &Ink) -> El {
    row()
        .w(34.0)
        .h(22.0)
        .px(3.0)
        .radius(11.0)
        .items_center()
        .fill(if on { ink.accent } else { ink.dim })
        .children(on.then(|| el().grow(1.0)))
        .child(
            el().size(16.0, 16.0)
                .radius(8.0)
                .fill(if on { ink.on_accent } else { ink.raised }),
        )
}

pub(in crate::ui) fn toggle(
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
        .min_h(60.0)
        .px(16.0)
        .py(10.0)
        .gap(12.0)
        .items_center()
        .focusable(true)
        .role("switch")
        .label(title)
        .description(detail)
        .checked(on)
        .hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(press)
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(3.0)
                .child(
                    text(title)
                        .font(TextStyle::new(13.0, 500.0))
                        .color(ink.text)
                        .wrap(),
                )
                .child(text(detail).font(CAPTION.lh(15.0)).color(ink.dim).wrap())
                .a11y_hidden(),
        )
        .child(switch(on, ink).a11y_hidden())
}

pub(super) fn stepper(desk: &Desk, ink: &Ink) -> El {
    let step = |glyph: &'static str, name, key: &'static str, press: fn(&mut Desk)| {
        pressable(key, ink)
            .size(44.0, 44.0)
            .focusable(true)
            .radius(8.0)
            .center()
            .border(1.0, ink.separator)
            .label(name)
            .on_press(press)
            .child(
                text(glyph)
                    .font(TextStyle::new(18.0, 450.0))
                    .color(ink.text)
                    .a11y_hidden(),
            )
    };
    let size = format!("{:.0} pt", desk.font_size);
    row()
        .key("text-size")
        .w_full()
        .min_h(60.0)
        .px(16.0)
        .py(8.0)
        .gap(4.0)
        .items_center()
        .label("Text size")
        .focusable(true)
        .range_value(
            f64::from(desk.font_size),
            f64::from(MIN_FONT_SIZE),
            f64::from(MAX_FONT_SIZE),
        )
        .range_step(1.0)
        .a11y_value(size.clone())
        .on_step(|d: &mut Desk, steps, _| d.zoom(Some(d.font_size + steps as f32)))
        .on_step_limit(|d: &mut Desk, max, _| {
            d.zoom(Some(if max { MAX_FONT_SIZE } else { MIN_FONT_SIZE }))
        })
        .child(
            text("Text size")
                .font(LABEL)
                .color(ink.text)
                .grow(1.0)
                .a11y_hidden(),
        )
        .child(step("−", "Smaller text", "text-smaller", |d| {
            d.zoom(Some(d.font_size - 1.0))
        }))
        .child(
            el().w(36.0)
                .center()
                .child(text(size).font(CAPTION).color(ink.text).a11y_hidden()),
        )
        .child(step("+", "Larger text", "text-bigger", |d| {
            d.zoom(Some(d.font_size + 1.0))
        }))
}

pub(super) fn preview(desk: &Desk, ink: &Ink) -> El {
    let (bg, fg, accent) = desk
        .settings
        .theme
        .map_or((ink.bg, ink.text, ink.accent), |t| {
            (color(t.background), color(t.foreground), t.roles().accent)
        });
    col()
        .key("settings-preview")
        .w_full()
        .gap(8.0)
        .child(section("Preview", ink).px(16.0))
        .child(
            col()
                .w_full()
                .padding(20.0)
                .gap(8.0)
                .radius(14.0)
                .fill(bg)
                .border(1.0, ink.separator)
                .label("Terminal preview")
                .child(
                    text("~/workspace  ·  main")
                        .font(CAPTION.family(Family::SystemMono))
                        .color(fg),
                )
                .child(
                    text("❯ phux attach")
                        .font(TextStyle::new(14.0, 500.0).family(Family::SystemMono))
                        .color(accent),
                )
                .child(
                    text("Your sessions, right where you left them.")
                        .font(CAPTION.family(Family::SystemMono))
                        .color(fg)
                        .wrap(),
                ),
        )
        .child(
            text("One theme for the interface and terminals. Programs can set their own colours.")
                .font(CAPTION)
                .color(ink.dim)
                .wrap()
                .px(16.0),
        )
}
