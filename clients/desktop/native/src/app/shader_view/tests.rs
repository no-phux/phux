use super::*;

fn fleet(working: u32, needs_you: u32) -> Fleet {
    Fleet {
        working,
        needs_you,
        agents: working + needs_you,
        ..Fleet::default()
    }
}

#[test]
fn energy_eases_toward_the_working_count_and_settles() {
    let mut signals = Signals::default();
    let busy = fleet(3, 0);
    signals.advance(busy, 0.1, true);
    assert!(signals.energy > 0. && signals.energy < Signals::target(3));
    assert!(signals.settling(busy));
    for _ in 0..200 {
        signals.advance(busy, 0.05, true);
    }
    assert_eq!(signals.energy, Signals::target(3));
    assert!(!signals.settling(busy));
    assert!(Signals::target(1) > 0.3 && Signals::target(1) < 0.4);
    assert!(Signals::target(5) > 0.85);
    assert_eq!(Signals::target(0), 0.);
}

#[test]
fn the_pulse_fires_when_more_agents_need_you_and_fades_in_two_seconds() {
    let mut signals = Signals::default();
    signals.advance(fleet(1, 0), 0.03, true);
    assert_eq!(signals.pulse, 0.);
    signals.advance(fleet(1, 1), 0.0, true);
    assert_eq!(signals.pulse, 1.);
    for _ in 0..20 {
        signals.advance(fleet(1, 1), 0.1, true);
    }
    assert!(signals.pulse < 0.05, "faded: {}", signals.pulse);
    // Fewer agents needing you never flashes.
    let mut calm = Signals::default();
    calm.advance(fleet(0, 2), 0., true);
    calm.pulse = 0.;
    calm.advance(fleet(0, 1), 0.03, true);
    assert_eq!(calm.pulse, 0.);
}

#[test]
fn without_motion_energy_lands_and_nothing_flashes() {
    let mut signals = Signals::default();
    signals.advance(fleet(4, 2), 0.01, false);
    assert_eq!(signals.energy, Signals::target(4));
    assert_eq!(signals.pulse, 0.);
    assert!(!signals.settling(fleet(4, 2)));
}

#[test]
fn pixels_keep_the_aspect_and_two_frames_fit_one_atlas_texture() {
    assert_eq!(pixels((800., 500.), 1.), (800, 500));
    assert_eq!(pixels((1600., 1000.), 1.), (819, 512));
    assert_eq!(pixels((2560., 1000.), 1.), (1024, 400));
    assert_eq!(pixels((600., 1400.), 1.), (439, 1024));
    assert_eq!(pixels((128., 76.), 2.), (256, 152));
    assert_eq!(pixels((0., 0.), 1.), (1, 1));
}
