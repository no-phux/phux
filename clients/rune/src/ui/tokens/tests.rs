use std::rc::Rc;

use phux_design::{Mode, Roles};

use super::*;

mod shadows;

fn system(dark: bool) -> Rc<Tokens> {
    Tokens::resolve(&Inputs::system(dark))
}

/// Nesting, as the chrome's surfaces nest: a menu's items inside its
/// `md` panel by `s1` land on `sm`; anything inset past its outer radius
/// clamps to `xs`.
#[test]
fn nested_radii_run_parallel() {
    let t = system(true);
    let (r, s) = (&t.radius, &t.space);
    assert!((t.nested(r.md, s.s1) - r.sm).abs() < f32::EPSILON);
    assert!((t.nested(r.lg, s.sheet_pad) - r.xs).abs() < f32::EPSILON);
    for (outer, inset) in [(r.lg, s.s1), (r.lg, s.card_inset), (r.xl, s.s2)] {
        let inner = t.nested(outer, inset);
        assert!(inner <= outer - inset || (inner - r.xs).abs() < f32::EPSILON);
        assert!(inner >= r.xs);
    }
}

#[test]
fn increased_contrast_draws_the_solved_variant() {
    let t = Tokens::resolve(&Inputs {
        increased_contrast: true,
        ..Inputs::system(true)
    });
    assert_eq!(t.roles, Roles::house_contrast_more(Mode::Dark));
    assert!(!t.glass_allowed());
    assert_ne!(t.version, system(true).version);
}

#[test]
fn a_theme_derives_its_roles() {
    let nord = crate::theme::find("nord").unwrap();
    let t = Tokens::resolve(&Inputs {
        theme: Some(*nord),
        ..Inputs::system(false)
    });
    assert_eq!(t.roles.layer.ground, Rgb8::hex(0x2e_3440).opaque());
    assert!(t.dark(), "the theme's own mode beats the system's");
}

#[test]
fn every_tier_springs_whole_and_glow_replaces_the_second_drop() {
    let t = system(true);
    for tier in [Tier::Raised, Tier::Floating, Tier::Overlay] {
        let layers = t.tiers.of(tier);
        assert!(layers.len() <= 4, "{tier:?}");
        let glowing = t
            .tiers
            .glowing(tier, t.status.tone(phux_design::Tone::NeedsYou).ink);
        assert!(glowing.len() <= 4, "{tier:?}");
        assert!(
            glowing
                .iter()
                .any(|s| s.blur > 0.0 && s.color.r > s.color.b),
            "a warm glow"
        );
    }
    assert!(t.tiers.of(Tier::Flat).is_empty());
    let ring = t
        .tiers
        .raised
        .iter()
        .find(|s| s.spread > 0.0 && s.blur == 0.0);
    assert_eq!(ring.map(|s| s.color), Some(t.roles.border.base.paint()));
}

#[test]
fn type_roles_follow_the_text_scale() {
    let plain = system(true);
    let big = Tokens::resolve(&Inputs {
        type_scale: 1.5,
        ..Inputs::system(true)
    });
    assert!((big.ty.body.size - plain.ty.body.size * 1.5).abs() < 1e-4);
    assert!((big.kit.ty.ui.size - big.ty.body.size).abs() < 1e-4);
    assert_eq!(plain.ty.mono.family, MONO);
    assert_ne!(big.version, plain.version);
}

#[test]
fn a_theme_changing_under_one_id_changes_the_version() {
    // Omarchy swaps its palette under the id "omarchy": memos keyed on the
    // id went stale; keyed on the version they repaint.
    let base = *crate::theme::find("nord").unwrap();
    let swapped = crate::theme::Theme {
        background: 0x10_1010,
        ..base
    };
    let a = Tokens::resolve(&Inputs {
        theme: Some(base),
        ..Inputs::system(true)
    });
    let b = Tokens::resolve(&Inputs {
        theme: Some(swapped),
        ..Inputs::system(true)
    });
    assert_eq!(base.id, swapped.id);
    assert_ne!(a.version, b.version);
}

#[test]
fn every_state_has_a_mark_and_the_quiet_pair_differ_by_word() {
    let t = system(true);
    let seen = t.status.state(phux_design::State::DoneSeen);
    let idle = t.status.state(phux_design::State::Idle);
    assert!(std::ptr::eq(seen.icon, idle.icon));
    assert_ne!(seen.word, idle.word);
    let ended = t.status.state(phux_design::State::Ended);
    assert_eq!(ended.tone.ink, t.roles.text.faint.paint());
}

#[test]
fn forced_colors_draw_every_role_in_the_system_colours() {
    let sc = rune::SystemContrast {
        window: rune::Color::BLACK,
        text: rune::Color::WHITE,
        highlight: crate::theme::color(0x1a_ebff),
        highlight_text: rune::Color::BLACK,
        disabled: crate::theme::color(0x3f_f23f),
        button: rune::Color::BLACK,
        button_text: rune::Color::WHITE,
        link: crate::theme::color(0xff_ff00),
    };
    let t = Tokens::resolve(&Inputs {
        forced: Some(forced::Forced::from(sc)),
        ..Inputs::system(true)
    });
    let r = &t.roles;
    assert_eq!(r.layer.raised.to_hex(), "#000000");
    assert_eq!(r.text.text.to_hex(), "#FFFFFF");
    assert_eq!(
        r.status.needs_you, r.text.text,
        "status keeps mark and word, in text"
    );
    assert_eq!(r.wash.hover.a, 0, "washes drop");
    assert_eq!(r.accent.accent.to_hex(), "#1AEBFF");
    assert!(t.tiers.of(Tier::Overlay).is_empty(), "shadows drop");
    assert!(!t.glass_allowed());
}

#[test]
fn glass_stops_at_the_verified_floor() {
    let t = Tokens::resolve(&Inputs {
        glass: 1.0,
        ..Inputs::system(true)
    });
    let floor = phux_design::tokens::MATERIAL.glass.tint_min.dark;
    assert!((t.glass_tint_alpha(1.0) - floor).abs() < 1e-6);
    assert!((t.glass_tint_alpha(0.0) - 1.0).abs() < 1e-6);
    let calm = Tokens::resolve(&Inputs {
        reduce_transparency: true,
        ..Inputs::system(true)
    });
    assert!(
        (calm.glass_tint_alpha(1.0) - 1.0).abs() < 1e-6,
        "reduce transparency is opaque"
    );
}

/// Every colour in `roles` as Rune paints it, read from the generated
/// groups' `Debug` form so a role a brand release adds is included.
fn role_colors(roles: &Roles) -> Vec<rune::Color> {
    let text = format!("{roles:?}");
    let mut out = Vec::new();
    for chunk in text.split("Rgba {").skip(1) {
        let field = |name: &str| -> u8 {
            let at = chunk.find(&format!("{name}: ")).unwrap() + name.len() + 2;
            let end = chunk[at..].find([',', ' ', '}']).unwrap();
            chunk[at..at + end].parse().unwrap()
        };
        let c = phux_design::Rgb8 {
            r: field("r"),
            g: field("g"),
            b: field("b"),
        };
        out.push(c.with_alpha_byte(field("a")).paint());
    }
    out
}

fn kit_parts(_: &(), cx: &mut rune::Cx<()>) -> rune::El {
    let t = Tokens::resolve(&Inputs::system(cx.system_dark()));
    let k = &t.kit;
    let mut layer = Vec::new();
    let menu = rune_kit::Menu::new("kit-menu")
        .standard()
        .open(true)
        .trigger(rune::text("Menu"))
        .item(rune_kit::MenuItem::new("Close"))
        .item(rune_kit::MenuItem::new("End session"))
        .build(k, &mut layer);
    let base = rune::col()
        .w_full()
        .h_full()
        .fill(t.roles.layer.ground.paint())
        .child(menu)
        .child(rune_kit::Progress::new(Some(0.4)).build(k))
        .child(
            rune_kit::shell::SidebarRow::new("phux")
                .subtitle("~/workspace")
                .build(k),
        )
        .child(
            rune_kit::shell::SidebarRow::new("site")
                .selected(true)
                .build(k),
        );
    rune_kit::root(base, layer)
}

#[test]
fn kit_components_paint_only_token_colours() {
    for dark in [true, false] {
        let t = system(dark);
        let mut allowed = role_colors(&t.roles);
        let c = &t.kit.c;
        // Kit-only mixes the tokens define: submit hover, the
        // contrast washes, the knob and its sheen, white and black.
        allowed.extend([
            c.submit_hover,
            c.contrast_hover,
            c.contrast_pressed,
            c.depth_top,
            c.depth_bottom,
            c.knob,
            rune::Color::WHITE,
            rune::Color::BLACK,
        ]);
        let close = |a: rune::Color, b: rune::Color| {
            (a.r - b.r).abs() < 2e-3
                && (a.g - b.g).abs() < 2e-3
                && (a.b - b.b).abs() < 2e-3
                && (a.a - b.a).abs() < 2e-3
        };
        let known = |x: rune::Color| x.a == 0.0 || allowed.iter().any(|a| close(*a, x));
        let mut rt =
            crate::test_support::runtime((), kit_parts, (800.0, 600.0), rune::Env::new(dark, 1.0));
        for at in [0.0, 1.0, 2.0] {
            rt.frame(at);
        }
        let elements = rt.elements();
        assert!(
            elements
                .iter()
                .any(|e| e.text.as_deref() == Some("End session")),
            "the menu opened"
        );
        let painted = elements.iter().filter(|e| e.fill.a > 0.0).count();
        assert!(painted >= 4, "the parts paint fills: {painted}");
        for e in &elements {
            assert!(
                known(e.fill),
                "dark {dark}: {} fills {}",
                e.path,
                e.fill.hex()
            );
            if let Some(ink) = e.text_color {
                assert!(known(ink), "dark {dark}: {} inks {}", e.path, ink.hex());
            }
        }
    }
}

/// Omarchy's Vantablack: every colour a grey, so `failed` is too.
const VANTABLACK: crate::theme::Theme = crate::theme::Theme {
    id: "vantablack",
    name: "Vantablack",
    dark: true,
    ansi: [
        0x00_0000, 0xa4_a4a4, 0xb6_b6b6, 0xce_cece, 0x8d_8d8d, 0x9b_9b9b, 0xb0_b0b0, 0xff_ffff,
        0x7a_7a7a, 0xa4_a4a4, 0xb6_b6b6, 0xce_cece, 0x8d_8d8d, 0x9b_9b9b, 0xb0_b0b0, 0xff_ffff,
    ],
    foreground: 0xff_ffff,
    background: 0x00_0000,
    cursor: 0xff_ffff,
    accent: 0x8d_8d8d,
    selection: Some(0x1a_1a1a),
    bold: None,
};

/// A grey `failed` cannot say danger, only look disabled, so a button that
/// ends something takes body text there; a hued one keeps the danger ink.
#[test]
fn a_grey_failed_tone_never_greys_out_the_verb() {
    use rune_kit::ButtonVariant;
    let grey = Tokens::resolve(&Inputs {
        theme: Some(VANTABLACK),
        ..Inputs::system(true)
    });
    assert!(grey.roles.recedes(grey.roles.status.failed));
    assert_eq!(grey.danger_button(), ButtonVariant::Neutral);
    for dark in [true, false] {
        assert_eq!(system(dark).danger_button(), ButtonVariant::Danger);
    }
}

/// A focused pane's rim opens at `border.control`: on a black ground its
/// lift casts no visible shadow, so the rim alone must say which pane has
/// the keys. That edge clears 3:1 on the ground in both house looks and in
/// a black theme; a resting rim's `border.base` does not.
#[test]
fn a_focused_pane_rim_reads_without_its_shadow() {
    let lut = phux_design::Params::house().srgb_lut;
    let black = Tokens::resolve(&Inputs {
        theme: Some(VANTABLACK),
        ..Inputs::system(true)
    });
    for t in [system(true), system(false), black] {
        let ground = t.roles.layer.ground.rgb();
        let b = &t.roles.border;
        let contrast = |c: phux_design::Rgb8| phux_design::solve::contrast(c, ground, lut);
        assert!(contrast(b.control.rgb()) >= 3.0);
        assert!(contrast(phux_design::solve::over(b.base, ground)) < 3.0);
    }
}

/// A grey accent quieter than dim text would make every emphasis (a
/// palette match, an accent icon) the dimmest ink on screen, so it takes
/// body text; a hued accent keeps its ink.
#[test]
fn a_grey_accent_never_dims_what_it_emphasises() {
    let black = Tokens::resolve(&Inputs {
        theme: Some(VANTABLACK),
        ..Inputs::system(true)
    });
    assert_eq!(black.roles.accent.accent_text, black.roles.text.text);
    for dark in [true, false] {
        let house = system(dark);
        assert_eq!(
            house.roles.accent.accent_text,
            Roles::house(if dark { Mode::Dark } else { Mode::Light })
                .accent
                .accent_text
        );
    }
}
