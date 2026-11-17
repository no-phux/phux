use std::rc::Rc;

use phux_design::{Mode, Roles};
use rune::Color;

use super::*;

fn system(dark: bool) -> Rc<Tokens> {
    Tokens::resolve(&Inputs::system(dark, Color::WHITE))
}

#[test]
fn equal_inputs_share_one_tokens() {
    let a = system(true);
    assert!(Rc::ptr_eq(&a, &system(true)));
    assert!(!Rc::ptr_eq(&a, &system(false)));
    assert_ne!(a.version, system(false).version);
}

#[test]
fn no_theme_is_the_house_look_in_the_system_appearance() {
    for (dark, mode) in [(true, Mode::Dark), (false, Mode::Light)] {
        let t = system(dark);
        assert_eq!(t.roles, Roles::house(mode));
        assert_eq!(t.dark(), dark);
        assert_eq!(t.kit.dark, dark);
    }
}

#[test]
fn increased_contrast_draws_the_solved_variant() {
    let t = Tokens::resolve(&Inputs {
        increased_contrast: true,
        ..Inputs::system(true, Color::WHITE)
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
        ..Inputs::system(false, Color::WHITE)
    });
    let derived = phux_design::derive(&theme_input(nord), &phux_design::Params::house());
    assert_eq!(t.roles, derived);
    assert!(t.dark(), "the theme's own mode beats the system's");
}

#[test]
fn the_kit_palette_comes_from_the_roles() {
    let t = system(true);
    let (c, r) = (&t.kit.c, &t.roles);
    assert_eq!(c.page, r.layer.ground.paint());
    assert_eq!(c.layer_01, r.layer.raised.paint());
    assert_eq!(c.text_base, r.text.text.paint());
    assert_eq!(c.text_muted, r.text.dim.paint());
    assert_eq!(c.text_faint, r.text.faint.paint());
    assert_eq!(
        c.border_strong,
        r.border.control.paint(),
        "fields draw the control border"
    );
    assert_eq!(c.border_focus, r.border.focus.paint());
    assert_eq!(c.danger.fg, r.status.failed.paint());
    assert_eq!(c.danger.bg, r.status.failed_wash.paint());
    assert_eq!(c.warning.icon, r.status.needs_you.paint());
    assert_eq!(c.layer_03, r.flat.raised.wash.current.paint());
    assert!(t.kit.ring.is_none(), "rings are coloured in the tiers");
    assert!(!t.kit.invalid_text);
    assert_eq!(
        t.kit.radius.md,
        phux_design::tokens::RADIUS.sm,
        "kit md is a control"
    );
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
        ..Inputs::system(true, Color::WHITE)
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
        ..Inputs::system(true, Color::WHITE)
    });
    let b = Tokens::resolve(&Inputs {
        theme: Some(swapped),
        ..Inputs::system(true, Color::WHITE)
    });
    assert_eq!(base.id, swapped.id);
    assert_ne!(a.version, b.version);
}

#[test]
fn the_window_blends_in_srgb() {
    // `over` (§2.2) composites in sRGB, which is how this window blends.
    assert!(rune::theme::blends_srgb());
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
