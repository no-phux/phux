//! Read actual presented shadow stacks, not only the authored token vectors.
use super::*;

const TIERS: [Tier; 4] = [Tier::Flat, Tier::Raised, Tier::Floating, Tier::Overlay];

fn presented(element: rune::El) -> Vec<rune::Shadow> {
    let mut rt = crate::test_support::runtime(
        element.key("shadow-probe").size(80.0, 40.0),
        |element, _| element.clone(),
        (120.0, 80.0),
        rune::Env::new(true, 2.0),
    );
    rt.set_system_reduced_motion(true);
    rt.frame(0.0);
    rt.presented_pose("shadow-probe")
        .unwrap()
        .shadows()
        .to_vec()
}

#[test]
fn own_and_kit_present_css_stacks_back_to_front() {
    for dark in [false, true] {
        let t = system(dark);
        for tier in TIERS {
            let stored = t.tiers.of(tier);
            let expected: Vec<_> = stored.iter().rev().copied().collect();
            let kit = t
                .kit
                .shadow_layers(stored)
                .fold(rune::el(), rune::El::box_shadow);
            assert_eq!(presented(kit), expected, "Kit {dark}/{tier:?}");
            assert_eq!(
                presented(t.elevate(rune::el(), tier)),
                expected,
                "OWN {dark}/{tier:?}"
            );
        }
    }
}

#[test]
fn normal_glows_keep_their_authored_replacement_and_four_layer_limit() {
    for dark in [false, true] {
        let t = system(dark);
        let color = t.status.tone(phux_design::Tone::NeedsYou).glow;
        for tier in TIERS {
            let authored = t.tiers.glowing(tier, color);
            assert!(authored.len() <= 4);
            let expected: Vec<_> = authored.into_iter().rev().collect();
            assert_eq!(
                presented(t.elevate_glowing(rune::el(), tier, color)),
                expected,
                "{dark}/{tier:?}"
            );
        }
    }
}

#[test]
fn forced_colors_never_reintroduce_glows_for_any_tier_or_supplied_ink() {
    for dark in [false, true] {
        let t = Tokens::resolve(&Inputs {
            forced: Some(forced::Forced::from(
                crate::test_support::visual::forced_colors(dark),
            )),
            ..Inputs::system(dark)
        });
        for tier in TIERS {
            assert!(presented(t.elevate(rune::el(), tier)).is_empty());
            for color in [Color::TRANSPARENT, Color::WHITE, t.host] {
                assert!(
                    presented(t.elevate_glowing(rune::el(), tier, color)).is_empty(),
                    "{dark}/{tier:?}/{color:?}"
                );
            }
        }
    }
}

#[test]
fn transparent_glow_keeps_normal_elevation_without_inventing_a_black_halo() {
    for dark in [false, true] {
        let t = system(dark);
        for tier in TIERS {
            assert_eq!(
                presented(t.elevate_glowing(rune::el(), tier, Color::TRANSPARENT)),
                presented(t.elevate(rune::el(), tier)),
                "{dark}/{tier:?}"
            );
        }
    }
}
