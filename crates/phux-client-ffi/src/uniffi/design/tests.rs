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
        let solved = design_contrast_more(design_house(ffi, false), terminal_ansi(mode)).unwrap();
        assert_eq!(solved, design_house(ffi, true));
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
    let bar = IDENTITY.host_bar;
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
    let max = IDENTITY.host_ease_max;
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

#[test]
fn verify_and_the_surfaces_match_the_solver() {
    use phux_design::tokens::TERMINAL;
    for (ffi, t) in [
        (DesignMode::Dark, TERMINAL.dark),
        (DesignMode::Light, TERMINAL.light),
    ] {
        let house = DesignCheck {
            house: true,
            house_extras: true,
            cursor: Some(t.cursor.rgb().to_u32()),
            background: Some(t.bg.rgb().to_u32()),
            ..DesignCheck::default()
        };
        assert_eq!(
            design_verify(design_house(ffi, false), house).unwrap(),
            vec![]
        );
        let more = DesignCheck {
            house: true,
            contrast_more: true,
            warn: vec!["V3".to_owned()],
            ..DesignCheck::default()
        };
        let found = design_verify(design_house(ffi, true), more).unwrap();
        assert!(
            found.iter().all(|v| v.severity == DesignSeverity::Warning),
            "{found:?}"
        );
    }
    // A broken look is reported, with the generator's ids and surfaces.
    let mut broken = design_house(DesignMode::Dark, false);
    broken.text.faint = broken.layer.overlay;
    broken.wash.press = broken.wash.hover;
    let found = design_verify(broken.clone(), DesignCheck::default()).unwrap();
    let ids: Vec<_> = found
        .iter()
        .map(|v| format!("{} {}", v.id, v.role))
        .collect();
    assert!(ids.contains(&"V2 text.faint".to_owned()), "{ids:?}");
    assert!(ids.contains(&"V9 wash.press".to_owned()), "{ids:?}");
    let tiers = design_tier_surfaces(broken.clone()).unwrap();
    let floor = design_floor_surfaces(broken).unwrap();
    assert_eq!(
        tiers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["ground", "sidebar", "raised", "overlay"]
    );
    assert_eq!(floor.len(), 4 + 4 * 4);
    assert_eq!(floor[..4], tiers[..]);
    assert_eq!(floor[19].name, "overlay+selected");
}
