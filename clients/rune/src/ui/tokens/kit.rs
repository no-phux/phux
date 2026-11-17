//! A `rune_kit::Kit` built from the tokens (§4.1.1): every palette and
//! type field assigned in an exhaustive literal, so a kit release that adds
//! a field breaks this build instead of drawing an unset colour.

use phux_design::Roles;
use phux_design::solve::over;
use rune::{Color, TextStyle};
use rune_kit::{Elevation, Glyphs, Kit, Motion, Palette, Radius, StateColors, Type};

use super::{MONO, MotionPairs, Paint, Tier, Tiers, style};
use crate::ui::icons;

fn state(roles: &Roles, tone: phux_design::Tone) -> StateColors {
    let t = roles.tone(tone);
    StateColors {
        bg: t.wash.paint(),
        fg: t.ink.paint(),
        border: t.edge.paint(),
        icon: t.ink.paint(),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one line per kit palette field, as the spec's table"
)]
fn palette(r: &Roles, tiers: &Tiers) -> Palette {
    use phux_design::Tone;
    let p = |c: phux_design::Rgba8| c.paint();
    let raised = &r.flat.raised;
    let overlay = &r.flat.overlay;
    let accent = r.accent.accent;
    let hover_a = f32::from(r.wash.hover.a) / 255.0;
    let press_a = f32::from(r.wash.press.a) / 255.0;
    let inverse = p(r.text.inverse);
    let submit_hover = over(
        r.accent.on_accent.rgb().with_alpha_byte(r.wash.hover.a),
        accent.rgb(),
    );
    let lit = (tiers.of(Tier::Raised).iter())
        .find(|s| s.inset)
        .map_or(Color::TRANSPARENT, |s| s.color);
    Palette {
        page: p(r.layer.ground),
        bg_base: p(r.layer.ground),
        bg_deep: p(r.layer.sidebar),
        layer_01: p(r.layer.raised),
        layer_02: p(r.layer.overlay),
        layer_03: p(raised.wash.current),
        layer_04: p(overlay.wash.press),
        bg_inverse: p(r.text.text),
        bg_contrast: p(r.text.text),
        bg_button_neutral: p(r.layer.raised),
        bg_icon_button_contrast: p(r.text.text),
        bg_accent: p(accent),
        submit_highlight: p(r.accent.hi),
        submit_hover: submit_hover.paint(),
        submit_fg: p(r.accent.on_accent),
        submit_icon: p(r.accent.on_accent),
        text_base: p(r.text.text),
        text_muted: p(r.text.dim),
        text_faint: p(r.text.faint),
        text_inverse: inverse,
        text_contrast: inverse,
        text_accent: p(r.accent.accent_text),
        icon_base: p(r.text.text),
        icon_muted: p(r.text.dim),
        icon_faint: p(r.text.faint),
        icon_inverse: inverse,
        icon_contrast: inverse,
        icon_accent: p(r.accent.accent_text),
        border_muted: p(r.border.muted),
        border_base: p(r.border.base),
        border_strong: p(r.border.control),
        border_focus: p(r.border.focus),
        hover: p(r.wash.hover),
        pressed: p(r.wash.press),
        contrast_hover: Color {
            a: hover_a,
            ..inverse
        },
        contrast_pressed: Color {
            a: press_a,
            ..inverse
        },
        scrim: p(r.layer.scrim),
        depth_top: lit,
        depth_bottom: tiers.drop_color(Tier::Raised),
        white: Color::WHITE,
        black: Color::BLACK,
        knob: Color::WHITE,
        knob_on: p(r.accent.on_accent),
        success: state(r, Tone::Done),
        warning: state(r, Tone::NeedsYou),
        danger: state(r, Tone::Failed),
        info: state(r, Tone::Working),
        surface_base: p(r.layer.raised),
        surface_weak: p(r.layer.sidebar),
        surface_hover: p(raised.wash.hover),
        surface_raised: p(r.layer.overlay),
        surface_raised_base: p(r.layer.raised),
        surface_raised_hover: p(overlay.wash.hover),
        surface_float: p(r.layer.overlay),
        surface_disabled: p(r.layer.sidebar),
        border_weak: p(r.border.muted),
        border_hover: p(r.border.strong),
        border_active: p(r.border.control),
        border_disabled: p(r.border.muted),
        text_strong: p(r.text.text),
        text_weak: p(r.text.dim),
        text_invert_strong: inverse,
        icon_weaker: p(r.text.faint),
        std_border: p(r.border.base),
        std_icon: p(r.text.dim),
        std_icon_invert: inverse,
        std_icon_strong: p(r.text.text),
        std_text: p(r.text.text),
    }
}

/// §4.1.1's type table, at `scale`.
fn type_scale(scale: f32) -> Type {
    use phux_design::tokens::TYPE;
    let r = &TYPE.role;
    let body = style(&r.body, scale);
    let caption = style(&r.caption, scale);
    let lead = style(&r.lead, scale);
    let label = style(&r.label, scale);
    let w = |s: TextStyle, weight: f32| TextStyle { weight, ..s };
    Type {
        ui: body,
        ui_medium: w(body, 500.0),
        ui_compact: body.lh(16.0 * scale),
        small: w(caption, 400.0),
        small_medium: w(caption, 500.0),
        body: w(lead, 400.0),
        body_medium: w(lead, 500.0),
        text_small: w(body, 400.0),
        text_small_medium: w(body, 500.0),
        text_xsmall: w(label, 400.0),
        text_xsmall_medium: w(label, 500.0),
        mono: TextStyle {
            family: MONO,
            ..label
        },
    }
}

/// The phux icons the kit's components draw.
const GLYPHS: Glyphs = Glyphs {
    check: &icons::CHECK,
    close: &icons::CLOSE,
    chevron_down: &icons::CHEVRON,
    caret_right: &icons::CHEVRON_RIGHT,
    search: &icons::SEARCH,
    lock: &icons::LOCK,
    ..rune_kit::glyphs::DEFAULT
};

/// The kit the chrome's selection controls build with (kit `Switch`,
/// `Checkbox`, `Progress`): `kit` with each control's boundary drawn in
/// `border.control`, opaque and at least 3:1 on every surface (§1.2,
/// WCAG 1.4.11), and a checked box filled with the accent under an
/// `on_accent` tick. `disabled` also dims a control's own label to
/// `text.faint`, as every disabled OWN control is (§1.10).
pub(super) fn controls(kit: &Kit, roles: &Roles, disabled: bool) -> Kit {
    let mut k = kit.clone();
    let control = roles.border.control.paint();
    let accent = roles.accent.accent.paint();
    // The off checkbox's outline, the progress track's edge, the standard
    // switch's off track.
    k.c.border_weak = control;
    // A checked box: the accent, its edge and an `on_accent` tick.
    k.c.surface_weak = accent;
    k.c.std_border = accent;
    k.c.std_icon = roles.accent.on_accent.paint();
    // The off switch track: a hairline ring of the control boundary over
    // its own fill, so the track reads on paper as on ink.
    k.elevation.switch_off = vec![rune::Shadow {
        x: 0.0,
        y: 0.0,
        blur: 0.0,
        spread: phux_design::tokens::SPACE.hairline,
        color: control,
        inset: true,
    }];
    if disabled {
        k.c.std_text = roles.text.faint.paint();
    }
    k
}

/// The kit for `roles`.
pub(super) fn build(roles: &Roles, tiers: &Tiers, motion: &MotionPairs, scale: f32) -> Kit {
    #[allow(
        clippy::disallowed_methods,
        reason = "the one place a Kit is made: every field is then assigned from tokens"
    )]
    let mut kit = Kit::neutral(roles.mode.is_dark());
    let r = phux_design::tokens::RADIUS;
    kit.dark = roles.mode.is_dark();
    kit.c = palette(roles, tiers);
    kit.ty = type_scale(scale);
    // Kit's `md` means "control" (buttons, fields, rows, menus); §4.1.1.
    kit.radius = Radius {
        xs: r.xs,
        sm: r.xs,
        md: r.sm,
        lg: r.md,
        xl: r.lg,
    };
    let mut elevation = Elevation::default();
    elevation.raised = tiers.of(Tier::Raised).to_vec();
    elevation.floating = tiers.of(Tier::Floating).to_vec();
    elevation.overlay = tiers.of(Tier::Overlay).to_vec();
    elevation.md = elevation.floating.clone();
    elevation.button_neutral = elevation.raised.clone();
    elevation.button_contrast = kit.elevation.button_contrast.clone();
    elevation.elements = kit.elevation.elements.clone();
    elevation.switch_off = kit.elevation.switch_off.clone();
    elevation.switch_on = kit.elevation.switch_on.clone();
    elevation.knob = kit.elevation.knob.clone();
    kit.elevation = elevation.with_input(tiers.of(Tier::Raised).to_vec());
    kit.motion = Motion {
        control: motion.control.enter,
        overlay: motion.pop.enter,
        disclosure: motion.list.enter,
    };
    kit.glyph = GLYPHS;
    // Rings are coloured when the tiers are built; a ring hook would also
    // repaint the lit edge, which has no blur either.
    kit.ring = None;
    kit.blend_over = None;
    kit.invalid_text = false;
    kit
}
