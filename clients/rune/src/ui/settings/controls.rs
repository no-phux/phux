//! The few reusable settings controls. Shared switches also serve Updates
//! and onboarding.

use rune::{El, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Switch};

use super::{primary, secondary, surface};
use crate::desk::Desk;
use crate::desk::INTERFACE_SIZES;
use crate::grid::{MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::ui::pressable;
use crate::ui::tokens::{Paint, Tokens};

/// The shortcut recorder's width: room for a chord's caps.
struct Geometry {
    recorder_w: f32,
}

const GEOMETRY: Geometry = Geometry { recorder_w: 150.0 };

/// A kit `Button` (§4.1.3 ADOPT) under phux's key, in the Tab order.
/// `Submit` is the one primary action per view; `Outline` the others.
pub(in crate::ui) fn kit_button(
    key: impl Into<String>,
    label: impl Into<String>,
    variant: ButtonVariant,
    t: &Tokens,
) -> El {
    Button::new(label)
        .variant(variant)
        .control_size(ControlSize::Large)
        .build(&t.kit)
        .key(key)
        .focusable(true)
        .min_h(grown(t.ty.body, t.space.s2))
}

/// The height a kit control needs for its label at this window's text
/// scale: kit control heights are fixed (AppKit metrics), so at a larger
/// text size the control grows to its label's `style` line plus `pad`
/// instead of clipping it. At the default scale this is the kit's height.
pub(in crate::ui) fn grown(style: rune::TextStyle, pad: f32) -> f32 {
    style.line_px() + pad
}

pub(in crate::ui) fn section(title: &'static str, t: &Tokens) -> El {
    text(title)
        .font(super::semibold(t.ty.label))
        .color(t.roles.text.dim.paint())
        .role("heading")
        .level(2)
}

/// A list of rows on a raised card (`radius.md`, `border.base`), hairline
/// rules between them inset to the row's text.
pub(super) fn card(rows: Vec<El>, t: &Tokens) -> El {
    let s = &t.space;
    let rule = t.roles.border.base.paint();
    let mut card = col()
        .w_full()
        .radius(t.radius.md)
        .clip_rounded(t.radius.md)
        .fill(surface(t))
        .border(s.hairline, rule);
    for (index, item) in rows.into_iter().enumerate() {
        if index > 0 {
            card = card.child(
                el().h(s.hairline)
                    .w_full()
                    .px(s.s4)
                    .child(el().w_full().h(s.hairline).fill(rule))
                    .a11y_hidden(),
            );
        }
        card = card.child(item);
    }
    card
}

/// A row's title over its detail, as a screen reader never hears them twice.
fn copy(desk: &Desk, title: &'static str, detail: &'static str, t: &Tokens) -> El {
    col()
        .grow(1.0)
        .min_w(0.0)
        .gap(t.space.s0_5)
        .child(
            text(title)
                .font(super::medium(primary(desk, t)))
                .color(t.roles.text.text.paint())
                .wrap(),
        )
        .child(
            text(detail)
                .font(secondary(desk, t))
                .color(t.roles.text.dim.paint())
                .wrap(),
        )
        .a11y_hidden()
}

/// A switch row: what it sets, and the kit switch that shows it.
pub(in crate::ui) struct Toggle {
    pub(in crate::ui) key: &'static str,
    pub(in crate::ui) title: &'static str,
    pub(in crate::ui) detail: &'static str,
    pub(in crate::ui) on: bool,
    pub(in crate::ui) press: fn(&mut Desk),
    /// Off, the row takes no press and draws the kit's disabled switch.
    pub(in crate::ui) enabled: bool,
}

/// The whole row is the switch (role, name, description, checked), so a
/// press anywhere on it flips it; the kit `Switch` draws the track and
/// stays out of the tree and the Tab order.
pub(in crate::ui) fn toggle(desk: &Desk, toggle: Toggle, t: &Tokens) -> El {
    let s = &t.space;
    let size = if desk.phone.is_some() {
        ControlSize::Small
    } else {
        ControlSize::Mini
    };
    let track = Switch::new(toggle.on)
        .label(toggle.title)
        .hide_label()
        .control_size(size)
        .disabled(!toggle.enabled)
        .build(&t.kit)
        .focusable(false)
        .a11y_hidden();
    let row = row()
        .key(toggle.key)
        .w_full()
        .min_h(s.touch_min)
        .px(s.s4)
        .py(s.s3)
        .gap(s.s3)
        .items_center()
        .role("switch")
        .label(toggle.title)
        .description(toggle.detail)
        .checked(toggle.on)
        .child(copy(desk, toggle.title, toggle.detail, t))
        .child(track);
    if !toggle.enabled {
        return row.disabled(true);
    }
    t.kit
        .interactive(row)
        .focusable(true)
        .cursor(rune::Cursor::Pointer)
        .on_press(toggle.press)
}

/// A row with a shortcut recorder: the quick terminal's chord, live, and
/// what holds it when it's refused. Off, or set in the file, it's disabled.
pub(super) fn shortcut(desk: &Desk, title: &'static str, detail: &'static str, t: &Tokens) -> El {
    let s = &t.space;
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
        .width(GEOMETRY.recorder_w)
        .disabled(!quick.on || desk.file.quick_terminal.is_some())
        .build(&t.kit);
    row()
        .key("quick-chord")
        .w_full()
        .min_h(s.touch_min)
        .px(s.s4)
        .py(s.s3)
        .gap(s.s3)
        .items_center()
        .child(copy(desk, title, detail, t))
        .child(recorder)
}

pub(super) fn text_size(desk: &Desk, t: &Tokens) -> El {
    stepper(
        desk,
        t,
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

pub(super) fn interface_size(desk: &Desk, t: &Tokens) -> El {
    let sizes = INTERFACE_SIZES;
    stepper(
        desk,
        t,
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

/// A stepper on the control outline, its value in `tnum`.
fn stepper(desk: &Desk, t: &Tokens, spec: Stepper) -> El {
    let s = &t.space;
    let button = |glyph: &'static str, name: String, key: String, by: i32| {
        let step = spec.step;
        pressable(key, t)
            .size(s.touch_min, s.touch_min)
            .focusable(true)
            .radius(t.radius.sm)
            .center()
            .border(s.hairline, t.roles.border.control.paint())
            .label(name)
            .on_press(move |d: &mut Desk| step(d, by))
            .child(
                text(glyph)
                    .font(t.ty.lead)
                    .color(t.roles.text.text.paint())
                    .a11y_hidden(),
            )
    };
    let lower = spec.title.to_lowercase();
    let (step, limit) = (spec.step, spec.limit);
    row()
        .key(format!("{}-size", spec.key))
        .w_full()
        .min_h(s.touch_min)
        .px(s.s4)
        .py(s.s2)
        .gap(s.s1)
        .items_center()
        .label(spec.title)
        .focusable(true)
        .range_value(spec.value, spec.range.0, spec.range.1)
        .range_step(1.0)
        .a11y_value(spec.shown.clone())
        .on_step(move |d: &mut Desk, steps, _| step(d, steps))
        .on_step_limit(move |d: &mut Desk, max, _| limit(d, max))
        .child(
            text(spec.title)
                .font(super::medium(primary(desk, t)))
                .color(t.roles.text.text.paint())
                .grow(1.0)
                .a11y_hidden(),
        )
        .child(button(
            "−",
            format!("Smaller {lower}").replace(" size", ""),
            format!("{}-smaller", spec.key),
            -1,
        ))
        .child(
            el().w(t.icon.button.lg).center().child(
                text(spec.shown)
                    .font(t.ty.micro)
                    .color(t.roles.text.text.paint())
                    .a11y_hidden(),
            ),
        )
        .child(button(
            "+",
            format!("Larger {lower}").replace(" size", ""),
            format!("{}-bigger", spec.key),
            1,
        ))
}

/// The chosen theme as a terminal shows it: its own ground and ink (terminal
/// content, not chrome), the prompt in the window's legible accent.
pub(super) fn preview(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let (bg, fg) =
        desk.settings
            .theme
            .map_or((r.layer.ground.paint(), r.text.text.paint()), |theme| {
                (
                    crate::theme::color(theme.background),
                    crate::theme::color(theme.foreground),
                )
            });
    let mono = |style: rune::TextStyle| rune::TextStyle {
        family: crate::ui::tokens::MONO,
        ..style
    };
    col()
        .key("settings-preview")
        .w_full()
        .gap(s.s2)
        .child(
            section("Preview", t)
                .font(super::heading_type(desk, t))
                .px(s.s4),
        )
        .child(
            col()
                .w_full()
                .padding(s.s5)
                .gap(s.s2)
                .radius(t.radius.md)
                .fill(bg)
                .border(s.hairline, r.border.base.paint())
                .label("Terminal preview")
                .child(
                    text("~/workspace  ·  main")
                        .font(mono(secondary(desk, t)))
                        .color(fg),
                )
                .child(
                    text("❯ phux attach")
                        .font(mono(super::medium(primary(desk, t))))
                        .color(r.accent.accent_text.paint()),
                )
                .child(
                    text("Your sessions, right where you left them.")
                        .font(mono(secondary(desk, t)))
                        .color(fg)
                        .wrap(),
                ),
        )
        .child(
            text("One theme for the interface and terminals. Programs can set their own colours.")
                .font(secondary(desk, t))
                .color(r.text.dim.paint())
                .wrap()
                .px(s.s4),
        )
}
