//! The Look step: colour scheme, sidebar, glass, and the theme strip.
//!
//! style-literals: illustration. The little windows and theme cards are
//! drawings in their own colours (an art module, §5.1); the chrome around
//! them (the choice rings, the groups and the glass slider) is on tokens.

use super::*;
use crate::ui::tokens::Voice;

/// The drawings' own sizes and colours: illustration constants.
mod art {
    use rune::Color;

    /// A little window.
    pub(super) const WINDOW: (f32, f32) = (72.0, 48.0);
    pub(super) const WINDOW_RADIUS: f32 = 9.0;
    pub(super) const SIDEBAR_W: f32 = 18.0;
    pub(super) const LIGHTS: f32 = 5.0;
    /// The Auto window's two halves.
    pub(super) const LIGHT_GROUND: u32 = 0xf2_f2_f4;
    pub(super) const DARK_GROUND: u32 = 0x1c_1c_21;
    /// A theme card.
    pub(super) const CARD: (f32, f32) = (112.0, 58.0);
    pub(super) const CARD_NAME_MAX: f32 = 118.0;
    pub(super) const DOT: f32 = 9.0;

    /// macOS's traffic lights.
    pub(super) fn lights() -> [Color; 3] {
        [
            rune::oklch(0.68, 0.19, 25.0),
            rune::oklch(0.82, 0.16, 85.0),
            rune::oklch(0.74, 0.17, 145.0),
        ]
    }
}

/// A titled group inside a panel.
fn group(title: &'static str, body: El, t: &Tokens) -> El {
    col()
        .gap(t.space.s2)
        .min_w(0.0)
        .child(
            text(title)
                .font(t.ty.label.semibold())
                .color(t.roles.text.dim.paint())
                .role("heading")
                .level(2),
        )
        .child(body)
}

/// A choice's selected ring: the accent at the focus ring's width, an
/// on/selected indicator (§1.2), around the art at `radius.md`.
fn ring(e: El, on: bool, t: &Tokens) -> El {
    let s = &t.space;
    e.radius(t.radius.md).padding(s.s0_5 + s.hairline).border(
        s.focus_ring.width,
        if on {
            t.roles.accent.accent.paint()
        } else {
            Color::TRANSPARENT
        },
    )
}

/// A pickable tile with a drawing and its word under it.
fn choice(
    key: &'static str,
    word: &'static str,
    on: bool,
    art: El,
    t: &Tokens,
    press: impl Fn(&mut Desk) + 'static,
) -> El {
    col()
        .gap(t.space.icon_gap)
        .items_center()
        .child(
            ring(crate::ui::pressable(key, t), on, t)
                .focusable(true)
                .role("radio")
                .label(word)
                .checked(on)
                .on_press(press)
                .child(art),
        )
        .child(
            text(word)
                .font(if on {
                    t.ty.label.semibold()
                } else {
                    t.ty.label
                })
                .color(
                    if on {
                        t.roles.text.text
                    } else {
                        t.roles.text.dim
                    }
                    .paint(),
                )
                .a11y_hidden(),
        )
}

/// A little window: a sidebar strip (when shown) and its traffic lights.
fn window(ground: Color, ink: Color, sidebar: bool) -> El {
    let dots = row()
        .gap(3.0)
        .children(art::lights().map(|c| el().size(art::LIGHTS, art::LIGHTS).radius(2.5).fill(c)));
    row()
        .size(art::WINDOW.0, art::WINDOW.1)
        .radius(art::WINDOW_RADIUS)
        .clip_rounded(art::WINDOW_RADIUS)
        .fill(ground)
        .border(1.0, ink.alpha(0.18))
        .children(sidebar.then(|| el().w(art::SIDEBAR_W).h_full().fill(ink.alpha(0.1))))
        .child(
            col()
                .grow(1.0)
                .h_full()
                .padding(6.0)
                .gap(5.0)
                .child(dots)
                .child(el().w_full().h(4.0).radius(2.0).fill(ink.alpha(0.25)))
                .child(el().w(24.0).h(4.0).radius(2.0).fill(ink.alpha(0.18))),
        )
}

pub(super) fn look(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let current = desk.settings.theme;
    let scheme = current.map(|t| t.dark);
    let (light, dark) = (
        theme::color(art::LIGHT_GROUND),
        theme::color(art::DARK_GROUND),
    );
    // One window, its right half in the dark: one set of lights, and the
    // bars running on across the split in the other ink. Two half windows
    // side by side each clipped and bordered their own corners.
    let half = art::WINDOW.0 / 2.0;
    let night = col()
        .pin(None, Some(0.0), Some(0.0), Some(0.0))
        .w(half)
        .fill(dark)
        .pt(6.0 + art::LIGHTS + 5.0)
        .pr(6.0)
        .child(el().w_full().h(4.0).fill(Color::WHITE.alpha(0.25)));
    let auto = window(light, Color::BLACK, false).child(night);
    let schemes = row()
        .gap(s.s3)
        .child(choice(
            "look-auto",
            "Auto",
            scheme.is_none(),
            auto,
            t,
            |d: &mut Desk| d.choose_scheme(None),
        ))
        .child(choice(
            "look-light",
            "Light",
            scheme == Some(false),
            window(light, Color::BLACK, false),
            t,
            |d: &mut Desk| d.choose_scheme(Some(false)),
        ))
        .child(choice(
            "look-dark",
            "Dark",
            scheme == Some(true),
            window(dark, Color::WHITE, false),
            t,
            |d: &mut Desk| d.choose_scheme(Some(true)),
        ));
    let shown = desk.sidebar_open;
    let (ground, ink) = (t.roles.layer.ground.paint(), t.roles.text.text.paint());
    let sidebar = row()
        .gap(s.s3)
        .child(choice(
            "look-sidebar",
            "Sidebar",
            shown,
            window(ground, ink, true),
            t,
            |d: &mut Desk| d.set_sidebar_open(true),
        ))
        .child(choice(
            "look-no-sidebar",
            "Hidden",
            !shown,
            window(ground, ink, false),
            t,
            |d: &mut Desk| d.set_sidebar_open(false),
        ));
    // The picker's own list, System first, narrowed to the chosen scheme.
    let themes = std::iter::once(None)
        .chain(theme::all().map(Some))
        .filter(|t| scheme.is_none_or(|dark| t.is_some_and(|t| t.dark == dark)));
    let strip = row()
        .key("look-themes")
        .w_full()
        .min_w(0.0)
        .scroll_x()
        .scroll_edge_fade(s.s4)
        .child(
            row()
                .gap(s.s3)
                .py(s.s0_5)
                .children(themes.map(|theme| theme_card(theme, current, t))),
        );
    col()
        .w_full()
        .items_center()
        .gap(s.s5)
        .child(heading(
            &icons::PALETTE,
            "Make phux yours",
            "Every pick shows on this window as you make it.",
            t,
        ))
        .child(
            panel(t)
                .child(
                    row()
                        .w_full()
                        .gap(s.s6)
                        .flex_wrap()
                        .child(group("Colour scheme", schemes, t))
                        .child(group("Sidebar", sidebar, t)),
                )
                .children(
                    cfg!(target_os = "macos")
                        .then(|| group("Glass", glass_slider(desk.settings.glass, t), t).w_full()),
                )
                .child(
                    el().w_full()
                        .h(s.hairline)
                        .fill(t.roles.border.base.paint()),
                )
                .child(group("Theme", strip, t).w_full()),
        )
}

/// Steps the arrow keys move the glass slider by, in percent.
const GLASS_STEP: u8 = 5;

/// The glass slider (an OWN slider: the kit has none): drag or click along
/// the track, or use the arrow keys. Every value applies to the window as
/// it changes, through §1.9's tint (`1 − g·(1 − tint_min)`), so the far end
/// is the thinnest glass `verify` allows, never clear.
fn glass_slider(percent: u8, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let at = f32::from(percent) / 100.0;
    let set_from = |d: &mut Desk, drag: &mut rune::Drag, _: &mut rune::Tx| {
        let at = (drag.x / drag.w.max(1.0)).clamp(0.0, 1.0);
        d.set_glass((at * 100.0).round() as u8);
    };
    let knob = t.icon.size.lg;
    let track = row()
        .w_full()
        .h(t.icon.button.md)
        .items_center()
        .child(
            el().grow(at)
                .h(s.icon_gap)
                .radius(t.radius.full)
                .fill(r.accent.accent.paint()),
        )
        .child(
            t.elevate(
                el().size(knob, knob)
                    .radius(t.radius.full)
                    .fill(Color::WHITE)
                    .border(s.hairline, r.border.control.paint()),
                Tier::Raised,
            ),
        )
        .child(
            el().grow(1.0 - at)
                .h(s.icon_gap)
                .radius(t.radius.full)
                .fill(r.border.control.paint()),
        );
    let caption = |words: &'static str| {
        text(words)
            .font(t.ty.caption)
            .color(r.text.dim.paint())
            .a11y_hidden()
    };
    row()
        .w_full()
        .gap(s.s3)
        .items_center()
        .child(caption("Solid"))
        .child(
            el().key("look-glass")
                .grow(1.0)
                .min_w(t.space.s16 * 2.0)
                .focusable(true)
                .role("slider")
                .label("Glass")
                .range_value(f64::from(percent), 0.0, 100.0)
                .cursor(rune::Cursor::Pointer)
                .on_drag(set_from)
                .on_key(move |d: &mut Desk, event: &rune::KeyEvent, _| {
                    let next = match event.key {
                        rune::Key::Left | rune::Key::Down => percent.saturating_sub(GLASS_STEP),
                        rune::Key::Right | rune::Key::Up => (percent + GLASS_STEP).min(100),
                        rune::Key::Home => 0,
                        rune::Key::End => 100,
                        _ => return false,
                    };
                    d.set_glass(next);
                    true
                })
                .child(track),
        )
        .child(caption("Glass"))
        .child(
            text(format!("{percent}%"))
                .font(t.ty.micro)
                .color(r.text.text.paint())
                .w(s.s10)
                .a11y_hidden(),
        )
}

/// A theme as a card in its own colours: `Aa`, a prompt in its accent, and
/// its palette as dots. `None` is System: the house look, its status tones
/// for dots.
fn theme_card(theme: Option<&'static Theme>, current: Option<&'static Theme>, t: &Tokens) -> El {
    let r = &t.roles;
    let (ground, ink, accent) = theme.map_or(
        (
            r.layer.ground.paint(),
            r.text.text.paint(),
            r.accent.accent_text.paint(),
        ),
        |theme| {
            (
                crate::theme::color(theme.background),
                crate::theme::color(theme.foreground),
                crate::theme::color(theme.accent),
            )
        },
    );
    let palette: Vec<Color> = theme.map_or_else(
        || {
            use phux_design::Tone;
            [
                Tone::Failed,
                Tone::Done,
                Tone::Working,
                Tone::NeedsYou,
                Tone::Idle,
                Tone::Bell,
            ]
            .map(|tone| t.status.tone(tone).ink)
            .to_vec()
        },
        |theme| {
            theme.ansi[1..7]
                .iter()
                .map(|c| crate::theme::color(*c))
                .collect()
        },
    );
    let on = theme.map(|t| t.id) == current.map(|t| t.id);
    let name = theme.map_or("System", |t| t.name);
    let art = col()
        .size(art::CARD.0, art::CARD.1)
        .padding(8.0)
        .gap(7.0)
        .radius(art::WINDOW_RADIUS)
        .fill(ground)
        .border(1.0, ink.alpha(0.16))
        .child(
            row()
                .gap(8.0)
                .items_center()
                .child(text("Aa").font(t.ty.lead.semibold()).color(ink))
                .child(
                    el().px(4.0).radius(4.0).fill(accent.alpha(0.22)).child(
                        text("~/")
                            .font(TextStyle {
                                family: crate::ui::tokens::MONO,
                                ..t.ty.caption
                            })
                            .color(accent),
                    ),
                ),
        )
        .child(
            row().gap(3.0).children(
                palette
                    .into_iter()
                    .map(|c| el().size(art::DOT, art::DOT).radius(4.5).fill(c)),
            ),
        );
    let key = format!("look-theme-{}", theme.map_or("system", |t| t.id));
    col()
        .gap(t.space.s1)
        .items_center()
        .child(
            ring(crate::ui::pressable(key, t), on, t)
                .focusable(true)
                .role("radio")
                .label(name)
                .checked(on)
                .on_press(move |d: &mut Desk| d.choose_theme(theme))
                .child(art.a11y_hidden()),
        )
        .child(
            text(name)
                .font(if on {
                    t.ty.caption.semibold()
                } else {
                    t.ty.caption
                })
                .color(if on { r.text.text } else { r.text.dim }.paint())
                .max_w(art::CARD_NAME_MAX)
                .truncate()
                .a11y_hidden(),
        )
}
