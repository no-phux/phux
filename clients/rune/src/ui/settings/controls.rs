//! The few reusable settings controls. Shared switches also serve Updates.

use rune::{El, Family, TextStyle, col, el, row, text};

use super::super::{CAPTION, Ink, LABEL, pressable};
use crate::desk::Desk;
use crate::desk::INTERFACE_SIZES;
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
    desk: &Desk,
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
                        .font(super::primary(desk, TextStyle::new(13.0, 500.0)))
                        .color(ink.text)
                        .wrap(),
                )
                .child(
                    text(detail)
                        .font(super::secondary(desk, CAPTION.lh(15.0)))
                        .color(ink.dim)
                        .wrap(),
                )
                .a11y_hidden(),
        )
        .child(switch(on, ink).a11y_hidden())
}

/// A row with a shortcut recorder: the quick terminal's chord, live, and
/// what holds it when it's refused. Off, or set in the file, it's dimmed.
pub(super) fn shortcut(desk: &Desk, title: &'static str, detail: &'static str, ink: &Ink) -> El {
    let quick = &desk.quick_chord;
    let owner = quick.owner.clone();
    let words = move |error: &rune::ShortcutError, chord: Option<&rune::Shortcut>| {
        crate::desk::quick::refusal(error, chord, owner.as_deref())
    };
    let recorder =
        rune_kit::ShortcutRecorder::new("set-quick-chord", &quick.recorder, |d: &mut Desk| {
            &mut d.quick_chord.recorder
        })
        .label(title)
        .on_change(|d: &mut Desk, next, _| d.record_quick_chord(next))
        .error_message(words)
        .width(150.0)
        .disabled(!quick.on || desk.file.quick_terminal.is_some())
        .build(&kit(ink));
    row()
        .key("quick-chord")
        .w_full()
        .min_h(60.0)
        .px(16.0)
        .py(10.0)
        .gap(12.0)
        .items_center()
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(3.0)
                .child(
                    text(title)
                        .font(super::primary(desk, TextStyle::new(13.0, 500.0)))
                        .color(ink.text)
                        .wrap(),
                )
                .child(
                    text(detail)
                        .font(super::secondary(desk, CAPTION.lh(15.0)))
                        .color(ink.dim)
                        .wrap(),
                )
                .a11y_hidden(),
        )
        .child(recorder)
}

/// The recorder's kit in this window's colours.
fn kit(ink: &Ink) -> rune_kit::Kit {
    let mut kit = rune_kit::Kit::neutral(crate::theme::on_fill(ink.bg) == rune::Color::WHITE);
    let c = &mut kit.c;
    (c.bg_base, c.border_base, c.border_focus) = (ink.bg, ink.separator, ink.accent);
    (c.text_muted, c.text_faint, c.layer_03) = (ink.text, ink.dim, ink.hover);
    (c.danger.fg, c.danger.border) = (ink.danger, ink.danger);
    kit
}

pub(super) fn text_size(desk: &Desk, ink: &Ink) -> El {
    stepper(
        desk,
        ink,
        Stepper {
            key: "text",
            title: "Text size",
            shown: format!("{:.0} pt", desk.font_size),
            value: f64::from(desk.font_size),
            range: (f64::from(MIN_FONT_SIZE), f64::from(MAX_FONT_SIZE)),
            step: |d, steps| d.zoom(Some(d.font_size + steps as f32)),
            limit: |d, max| d.zoom(Some(if max { MAX_FONT_SIZE } else { MIN_FONT_SIZE })),
        },
    )
}

pub(super) fn interface_size(desk: &Desk, ink: &Ink) -> El {
    let sizes = INTERFACE_SIZES;
    stepper(
        desk,
        ink,
        Stepper {
            key: "interface",
            title: "Interface size",
            shown: format!("{}%", desk.interface_size),
            value: f64::from(desk.interface_size),
            range: (f64::from(sizes[0]), f64::from(sizes[sizes.len() - 1])),
            step: Desk::step_interface_size,
            limit: |d, max| {
                d.set_interface_size(Some(if max {
                    INTERFACE_SIZES[7]
                } else {
                    INTERFACE_SIZES[0]
                }))
            },
        },
    )
}

/// A value with − and + beside it, which VoiceOver steps as one control.
struct Stepper {
    /// `{key}-size`, `{key}-smaller` and `{key}-bigger`.
    key: &'static str,
    title: &'static str,
    shown: String,
    value: f64,
    range: (f64, f64),
    step: fn(&mut Desk, i32),
    limit: fn(&mut Desk, bool),
}

fn stepper(desk: &Desk, ink: &Ink, s: Stepper) -> El {
    let button = |glyph: &'static str, name: String, key: String, by: i32| {
        let step = s.step;
        pressable(key, ink)
            .size(44.0, 44.0)
            .focusable(true)
            .radius(8.0)
            .center()
            .border(1.0, ink.separator)
            .label(name)
            .on_press(move |d: &mut Desk| step(d, by))
            .child(
                text(glyph)
                    .font(TextStyle::new(18.0, 450.0))
                    .color(ink.text)
                    .a11y_hidden(),
            )
    };
    let lower = s.title.to_lowercase();
    let (step, limit) = (s.step, s.limit);
    row()
        .key(format!("{}-size", s.key))
        .w_full()
        .min_h(60.0)
        .px(16.0)
        .py(8.0)
        .gap(4.0)
        .items_center()
        .label(s.title)
        .focusable(true)
        .range_value(s.value, s.range.0, s.range.1)
        .range_step(1.0)
        .a11y_value(s.shown.clone())
        .on_step(move |d: &mut Desk, steps, _| step(d, steps))
        .on_step_limit(move |d: &mut Desk, max, _| limit(d, max))
        .child(
            text(s.title)
                .font(super::primary(desk, LABEL))
                .color(ink.text)
                .grow(1.0)
                .a11y_hidden(),
        )
        .child(button(
            "−",
            format!("Smaller {lower}").replace(" size", ""),
            format!("{}-smaller", s.key),
            -1,
        ))
        .child(
            el().w(36.0).center().child(
                text(s.shown)
                    .font(super::secondary(desk, CAPTION))
                    .color(ink.text)
                    .a11y_hidden(),
            ),
        )
        .child(button(
            "+",
            format!("Larger {lower}").replace(" size", ""),
            format!("{}-bigger", s.key),
            1,
        ))
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
        .child(
            section("Preview", ink)
                .font(super::secondary(desk, TextStyle::new(12.0, 600.0)))
                .px(16.0),
        )
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
                        .font(super::secondary(desk, CAPTION).family(Family::Mono))
                        .color(fg),
                )
                .child(
                    text("❯ phux attach")
                        .font(
                            super::primary(desk, TextStyle::new(14.0, 500.0)).family(Family::Mono),
                        )
                        .color(accent),
                )
                .child(
                    text("Your sessions, right where you left them.")
                        .font(super::secondary(desk, CAPTION).family(Family::Mono))
                        .color(fg)
                        .wrap(),
                ),
        )
        .child(
            text("One theme for the interface and terminals. Programs can set their own colours.")
                .font(super::secondary(desk, CAPTION))
                .color(ink.dim)
                .wrap()
                .px(16.0),
        )
}
