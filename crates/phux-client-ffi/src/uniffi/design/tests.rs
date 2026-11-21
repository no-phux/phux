use phux_design::tokens::{IDENTITY, TERMINAL};
use phux_design::{Mode, Params, Rgb8, Roles};

use super::*;

fn terminal_ansi(mode: Mode) -> Vec<u32> {
    let t = if mode.is_dark() {
        TERMINAL.dark
    } else {
        TERMINAL.light
    };
    t.ansi.iter().map(|c| c.rgb().to_u32()).collect()
}

#[test]
fn every_mobile_theme_derives_as_the_solver_does() {
    let themes = design_catalog("mobile".to_owned());
    assert_eq!(
        themes.len(),
        phux_design::catalog().shipped("mobile").count()
    );
    assert_eq!(themes[0].id, "phux");
    for (got, want) in themes.iter().zip(phux_design::catalog().shipped("mobile")) {
        let roles = design_derive(got.theme.clone()).unwrap();
        assert_eq!(
            roles,
            phux_design::derive(&want.input(), &Params::house()).into(),
            "{}",
            got.id
        );
        assert_eq!(
            Roles::try_from(roles).unwrap(),
            phux_design::derive(&want.input(), &Params::house())
        );
    }
}

#[test]
fn the_house_look_and_its_variant_come_through_for_both_modes() {
    for (ffi, mode) in [
        (DesignMode::Dark, Mode::Dark),
        (DesignMode::Light, Mode::Light),
    ] {
        assert_eq!(design_house(ffi, false), Roles::house(mode).into());
        assert_eq!(
            design_house(ffi, true),
            Roles::house_contrast_more(mode).into()
        );
        // The solve reproduces the exported variant, but for the metal
        // accent's hi and lo, which only the house look authors.
        let mut solved =
            design_contrast_more(design_house(ffi, false), terminal_ansi(mode)).unwrap();
        let variant = design_house(ffi, true);
        (solved.accent.hi, solved.accent.lo) = (variant.accent.hi, variant.accent.lo);
        assert_eq!(solved, variant);
        assert_eq!(design_house(ffi, false).mode, ffi);
    }
}

#[test]
fn malformed_palettes_are_errors_not_panics() {
    let mut theme = design_catalog("mobile".to_owned()).remove(0).theme;
    theme.ansi.pop();
    assert_eq!(design_derive(theme), Err(DesignError::Ansi(15)));
    let mut roles = design_house(DesignMode::Dark, false);
    roles.syntax.push(roles.syntax[0]);
    assert_eq!(
        design_ease_host(0x00_0000, roles.clone()),
        Err(DesignError::Syntax(17))
    );
    assert_eq!(
        design_contrast_more(roles, vec![0; 16]),
        Err(DesignError::Syntax(17))
    );
}

#[test]
fn colour_maths_matches_the_golden_primitives() {
    assert!((design_contrast(0x77_7777, 0xFF_FFFF) - 4.478_089_453_577_214).abs() <= 1e-12);
    assert!(
        (design_contrast(0xFF_FFFF, 0x77_7777) - design_contrast(0x77_7777, 0xFF_FFFF)).abs()
            <= 0.0
    );
    assert_eq!(design_mix(0x0D_1017, 0xD8_DEE9, 0.065), 0x1A_1D25);
    assert!((design_luminance(0xFF_FFFF) - 1.0).abs() <= 1e-12);
    assert!(design_luminance(0x00_0000).abs() <= 0.0);
}

#[test]
fn a_host_colour_eases_just_far_enough_at_most_halfway() {
    let house = design_house(DesignMode::Dark, false);
    let roles = Roles::try_from(house.clone()).unwrap();
    let layers =
        [roles.layer.ground, roles.layer.sidebar, roles.layer.raised].map(phux_design::Rgba8::rgb);
    let lut = Params::house().srgb_lut;
    let bar = f64::from(IDENTITY.host_bar);
    let reads = |c: u32| phux_design::solve::worst(Rgb8::hex(c), &layers, lut) >= bar;
    // Already readable: unchanged.
    assert!(reads(0xE8_E5DF));
    assert_eq!(
        design_ease_host(0xE8_E5DF, house.clone()).unwrap(),
        0xE8_E5DF
    );
    // Too dark on the dark house: eased toward white until it reads.
    let eased = design_ease_host(0x20_3060, house).unwrap();
    assert!(reads(eased) && eased != 0x20_3060, "{eased:06X}");
    // Unreachable within the cap: the halfway point.
    let grey = DesignTheme {
        bg: 0x77_7777,
        fg: 0xFF_FFFF,
        ansi: vec![0x80_8080; 16],
        cursor: None,
        accent: None,
        selection_bg: None,
        dark: Some(false),
    };
    let flat = design_derive(grey).unwrap();
    let max = f64::from(IDENTITY.host_ease_max);
    assert_eq!(
        design_ease_host(0x77_7777, flat).unwrap(),
        design_mix(0x77_7777, 0x00_0000, max)
    );
}

#[test]
fn tone_of_and_plurals_speak_the_vocabulary() {
    let pane = |agent| DesignToneInput {
        agent: Some(agent),
        ..DesignToneInput::default()
    };
    assert_eq!(
        design_tone_of(pane(DesignAgent::Blocked)),
        DesignState::NeedsYou
    );
    assert_eq!(
        design_tone_of(DesignToneInput {
            exit: Some(1),
            ..pane(DesignAgent::Done)
        }),
        DesignState::Failed
    );
    assert_eq!(
        design_tone_of(DesignToneInput {
            link: Some(DesignLink::Lost),
            ..pane(DesignAgent::Working)
        }),
        DesignState::Lost
    );
    assert_eq!(
        design_tone_of(DesignToneInput {
            seen: true,
            ..pane(DesignAgent::Done)
        }),
        DesignState::DoneSeen
    );
    assert_eq!(
        design_tone_of(DesignToneInput {
            exit: Some(0),
            ..DesignToneInput::default()
        }),
        DesignState::Ended
    );
    assert_eq!(
        design_plural("count.needs_you".to_owned(), 1).as_deref(),
        Some("1 needs you")
    );
    assert_eq!(
        design_plural("count.needs_you".to_owned(), 2).as_deref(),
        Some("2 need you")
    );
    assert_eq!(design_plural("count.nope".to_owned(), 2), None);
    let message = "{count, plural, one {# done} other {# done}}".to_owned();
    assert_eq!(
        design_plural_message(message, 21).as_deref(),
        Some("21 done")
    );
}
