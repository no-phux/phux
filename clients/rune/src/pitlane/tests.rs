use super::sim::DT;
use super::*;
use crate::reef::CLAUDE;

fn driver(n: u32, host: usize, mood: Mood) -> Driver {
    Driver {
        id: ResourceId::local(n),
        name: format!("agent-{n}"),
        host,
        mood,
        species: &CLAUDE,
        work: None,
    }
}

fn hosts(names: &[&str]) -> Vec<(String, bool)> {
    names.iter().map(|n| ((*n).to_owned(), true)).collect()
}

fn scene(drivers: Vec<Driver>, names: &[&str]) -> PitLane {
    let mut s = PitLane::default();
    s.sync(drivers);
    s.layout(1280.0, 800.0, &hosts(names));
    s
}

#[test]
fn the_circuit_is_resampled_evenly_and_closes() {
    let s = scene(vec![driver(1, 0, Mood::Working)], &["mini"]);
    for w in s.trk.windows(2) {
        let d = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        assert!(d <= SPACING + 0.01, "{d}");
    }
    let (first, last) = (s.trk[0], s.trk[s.trk.len() - 1]);
    assert!((first[0] - last[0]).hypot(first[1] - last[1]) <= SPACING + 0.01);
    // Round the closed circuit, clamped at the ends of the open pit lane.
    let (a, _, _) = at(&s.trk, s.lap() + 1.0, true);
    let (b, _, _) = at(&s.trk, 1.0, true);
    assert!((a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01);
    assert_eq!(at(&s.pit, -50.0, false).0, s.pit[0]);
    assert_eq!(at(&s.pit, 1e6, false).0, s.pit[s.pit.len() - 1]);
}

#[test]
fn cars_brake_for_bends_not_on_the_main_straight() {
    let s = scene(vec![driver(1, 0, Mood::Working)], &["mini"]);
    assert!(s.slow.iter().all(|v| (0.42..=1.0).contains(v)));
    let straight = nearest(&s.trk, s.to_s(0.4, 0.1)) / SPACING;
    assert!(s.slow[straight as usize] > 0.95);
    let hairpin = nearest(&s.trk, s.to_s(0.03, 0.16)) / SPACING;
    assert!(s.slow[hairpin as usize] < 0.75);
}

#[test]
fn garages_follow_the_machines_order_whatever_needs_you() {
    let mut drivers = vec![
        driver(1, 0, Mood::Working),
        driver(2, 1, Mood::Idle),
        driver(3, 1, Mood::Working),
        driver(4, 2, Mood::Done),
    ];
    let mut s = scene(drivers.clone(), &["mini", "macbook", "powerhouse-1"]);
    let order = |s: &PitLane| -> Vec<(String, f32, usize)> {
        let g = s.garages.iter();
        g.map(|g| (g.host.clone(), g.u0, g.bays.len())).collect()
    };
    let before = order(&s);
    assert_eq!(
        before.iter().map(|g| g.0.as_str()).collect::<Vec<_>>(),
        ["mini", "macbook", "powerhouse-1"]
    );
    assert_eq!(before.iter().map(|g| g.2).collect::<Vec<_>>(), [1, 2, 1]);
    assert!(before.windows(2).all(|w| w[0].1 < w[1].1));
    // A host gaining a waiting agent keeps its garage where it was.
    drivers[3].mood = Mood::Waiting;
    s.sync(drivers);
    let rev = s.rev;
    s.layout(1280.0, 800.0, &hosts(&["mini", "macbook", "powerhouse-1"]));
    assert_eq!(order(&s), before);
    assert_eq!(s.rev, rev, "same agents, same layout");
}

#[test]
fn a_machine_without_agents_still_has_a_garage() {
    let s = scene(vec![driver(1, 0, Mood::Working)], &["mini", "far"]);
    let far = &s.garages[1];
    assert!(far.bays.is_empty() && far.u1 > far.u0);
    assert!(far.u1 <= 0.75, "clear of parc fermé");
}

#[test]
fn each_mood_has_its_spot() {
    let s = scene(
        vec![
            driver(1, 0, Mood::Working),
            driver(2, 0, Mood::Waiting),
            driver(3, 0, Mood::Idle),
            driver(4, 0, Mood::Done),
            driver(5, 0, Mood::Done),
        ],
        &["mini"],
    );
    assert_eq!(s.goal(0), None, "working laps");
    let bay = |n: u32| {
        s.garages[0]
            .bays
            .iter()
            .find(|b| b.id == ResourceId::local(n))
            .unwrap()
            .p
    };
    assert_eq!(s.goal(1), Some([bay(2), 0.035 * s.sp]));
    assert_eq!(s.goal(2), Some([bay(3), 0.13 * s.sp]));
    assert_eq!(s.goal(3), Some(s.parc[0]));
    assert_eq!(s.goal(4), Some(s.parc[1]));
    // Parc fermé is past every garage, down the lane.
    assert!(s.parc[0][0] > bay(3));
}

#[test]
fn higher_tiers_are_bigger_cars() {
    let mut s = scene(vec![driver(1, 0, Mood::Working)], &["mini"]);
    let base = s.cars[0].r(s.k);
    s.cars[0].tier = 4;
    assert!((s.cars[0].r(s.k) / base - 1.2 / 1.05).abs() < 1e-4);
}

/// Seconds until car 0 is in the pit lane, stepping at most `limit`.
fn seconds_to_box(s: &mut PitLane, limit: f32) -> f32 {
    let mut t = 0.0;
    while s.cars[0].mode == Mode::Track && t < limit {
        s.step(DT);
        t += DT;
    }
    t
}

#[test]
fn a_car_that_needs_you_boxes_well_inside_a_lap() {
    let mut s = scene(vec![driver(1, 0, Mood::Working)], &["mini"]);
    s.step(DT);
    // Just past the pit entry: the worst case, a whole lap to go.
    s.cars[0].d = (s.entry + 10.0) % s.lap();
    for _ in 0..120 {
        s.step(DT);
    }
    let mut racing = s.clone();
    let lap = {
        racing.cars[0].d = (s.entry + 10.0) % s.lap();
        let mut t = 0.0;
        let mut gone = 0.0;
        while gone < racing.lap() - 10.0 && t < 120.0 {
            let d = racing.cars[0].d;
            racing.step(DT);
            gone += (racing.cars[0].d - d + racing.lap()) % racing.lap();
            t += DT;
        }
        t
    };
    s.cars[0].d = (s.entry + 10.0) % s.lap();
    s.cars[0].who.mood = Mood::Waiting;
    let boxed = seconds_to_box(&mut s, 120.0);
    assert_eq!(s.cars[0].mode, Mode::Pit);
    assert!(boxed < lap * 0.8, "boxed in {boxed} s, a lap is {lap} s");
}

#[test]
fn no_car_drives_the_pit_lane_backwards() {
    let mut s = scene(
        vec![driver(1, 0, Mood::Done), driver(2, 0, Mood::Working)],
        &["mini"],
    );
    s.step(DT);
    assert!(s.cars[0].landed && s.cars[0].mode == Mode::Pit);
    // Done to idle: its garage is back up the lane.
    s.cars[0].who.mood = Mood::Idle;
    let mut p = s.cars[0].p;
    let mut t = 0.0;
    while s.cars[0].mode == Mode::Pit && t < 30.0 {
        s.step(DT);
        assert!(s.cars[0].p >= p - 1e-3, "rolled back up the lane");
        p = s.cars[0].p;
        t += DT;
    }
    assert_eq!(s.cars[0].mode, Mode::Track, "it rejoins the circuit");
    // And comes round to box at its garage.
    let mut t = 0.0;
    while !s.cars[0].landed && t < 120.0 {
        s.step(DT);
        t += DT;
    }
    assert!(s.cars[0].landed, "parked within two minutes");
    let goal = s.goal(0).unwrap();
    assert!((s.cars[0].p - goal[0]).abs() < 1.0 && (s.cars[0].q - goal[1]).abs() < 0.6);
}

#[test]
fn the_race_is_the_same_from_the_same_seed() {
    let run = || {
        let mut s = scene(
            (1..=8)
                .map(|n| driver(n, (n % 2) as usize, Mood::Working))
                .collect(),
            &["mini", "macbook"],
        );
        for _ in 0..300 {
            s.step(DT);
        }
        s.cars.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn a_resize_keeps_cars_on_the_circuit() {
    let mut s = scene(
        (1..=4).map(|n| driver(n, 0, Mood::Working)).collect(),
        &["mini"],
    );
    for _ in 0..60 {
        s.step(DT);
    }
    let frac = s.cars[0].d / s.lap();
    s.layout(900.0, 600.0, &hosts(&["mini"]));
    assert!((s.cars[0].d / s.lap() - frac).abs() < 1e-3);
}
