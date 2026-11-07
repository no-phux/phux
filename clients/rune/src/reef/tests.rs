use super::*;

fn swimmer(n: u32, host: usize, mood: Mood) -> Swimmer {
    Swimmer {
        id: ResourceId::local(n),
        session: "work".to_owned(),
        name: format!("agent-{n}"),
        host,
        mood,
        species: &CLAUDE,
        project: None,
        school: n as usize,
    }
}

fn hosts(names: &[&str]) -> Vec<(String, bool)> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| ((*n).to_owned(), i == 0))
        .collect()
}

#[test]
fn harnesses_map_to_species_and_the_rest_fall_back() {
    assert_eq!(species(Some("claude"), "x"), &CLAUDE);
    assert_eq!(species(Some("claude-code"), "x"), &CLAUDE);
    assert_eq!(species(Some("codex"), "x"), &CODEX);
    assert_eq!(species(Some("opencode"), "x"), &OPENCODE);
    assert_eq!(species(Some("gemini-cli"), "x"), &GEMINI);
    assert_eq!(
        species(None, "Gemini"),
        &GEMINI,
        "the name when no kind is declared"
    );
    for other in ["pi", "omp", "grok", "amp", "cursor", "goose", "aider", "fx"] {
        assert_eq!(species(Some(other), other), &OTHER);
    }
    assert_eq!(species(None, "build"), &OTHER);
}

#[test]
fn marks_become_the_four_moods() {
    assert_eq!(Mood::of(Mark::Blocked), Mood::Waiting);
    assert_eq!(Mood::of(Mark::Failed), Mood::Waiting);
    assert_eq!(Mood::of(Mark::Bell), Mood::Waiting);
    assert_eq!(Mood::of(Mark::Done), Mood::Done);
    assert_eq!(Mood::of(Mark::Succeeded), Mood::Done);
    assert_eq!(Mood::of(Mark::Working), Mood::Working);
    assert_eq!(Mood::of(Mark::Running), Mood::Working);
    assert_eq!(Mood::of(Mark::Idle), Mood::Idle);
    assert_eq!(Mood::of(Mark::Background), Mood::Idle);
    assert_eq!(Mood::of(Mark::None), Mood::Idle);
}

#[test]
fn crowded_reefs_draw_smaller_fish_down_to_a_floor() {
    assert_eq!(fish_scale(1), 1.0);
    assert_eq!(fish_scale(8), 1.0);
    assert!((fish_scale(16) - 0.5f32.sqrt()).abs() < 1e-6);
    assert_eq!(fish_scale(100), 0.55);
}

#[test]
fn zones_split_the_width_in_order_and_keep_their_coral() {
    let mut reef = Reef::default();
    reef.layout(1200.0, 780.0, &hosts(&["mini", "macbook", "powerhouse-1"]));
    let z = &reef.zones;
    assert_eq!(z.len(), 3);
    assert_eq!(
        (z[0].l, z[1].l, z[2].l, z[2].r),
        (0.0, 400.0, 800.0, 1200.0)
    );
    assert_eq!(z[1].cx, 600.0);
    assert!(z[0].known && !z[1].known);
    // The same host makes the same reef wherever it sits.
    let alone = zone("macbook", false, 0, 400.0, reef.s, reef.floor_y);
    assert_eq!(alone.rocks[0].rx, z[1].rocks[0].rx);
    assert!(((alone.vent_x - alone.cx) - (z[1].vent_x - z[1].cx)).abs() < 1e-3);
    assert_ne!(z[0].rocks[0].rx, z[1].rocks[0].rx);
    for zone in z {
        assert!((1..=2).contains(&zone.fans.len()) && (2..=3).contains(&zone.anemones.len()));
        assert_eq!((zone.corals.len(), zone.nooks.len()), (4, 6));
    }
    assert_eq!(hash("a"), 0xe40c292c, "FNV-1a, as the prototype");
}

#[test]
fn each_mood_has_its_place_in_its_reef() {
    let mut reef = Reef::default();
    reef.layout(1200.0, 780.0, &hosts(&["mini", "macbook"]));
    let (s, sy) = (reef.s, reef.surface_y);
    let z = &reef.zones[1];
    let up = target(Mood::Waiting, z, [0, 1], 0.0, 0.0, s, sy);
    assert!(
        (up[1] - (sy + 30.0 * s)).abs() < 3.5 * s,
        "just under the surface"
    );
    assert!(up[0] > z.l && up[0] < z.r);
    let home = target(Mood::Done, z, [0, 1], 0.0, 0.0, s, sy);
    assert!(
        home[1] < z.home_y && home[1] > z.home_y - 45.0 * s,
        "just above its reef"
    );
    let rest = target(Mood::Idle, z, [2, 3], 0.0, 0.0, s, sy);
    assert!(
        (rest[0] - z.nooks[2][0]).abs() <= 9.0 * s && (rest[1] - z.nooks[2][1]).abs() <= 2.0 * s
    );
    // Two waiting fish in one reef spread apart.
    let a = target(Mood::Waiting, z, [0, 2], 0.0, 0.0, s, sy);
    let b = target(Mood::Waiting, z, [1, 2], 0.0, 0.0, s, sy);
    assert!(b[0] - a[0] > 30.0 * s);
}

#[test]
fn the_fleet_becomes_fish_that_settle_where_their_mood_says() {
    let mut reef = Reef::default();
    reef.sync(vec![
        swimmer(1, 0, Mood::Working),
        swimmer(2, 0, Mood::Idle),
        swimmer(3, 1, Mood::Done),
    ]);
    reef.layout(1200.0, 780.0, &hosts(&["mini", "macbook"]));
    for _ in 0..600 {
        reef.step(1.0 / 60.0);
    }
    let z0 = reef.zones[0].clone();
    let work = &reef.fish[0];
    assert!(
        work.x > z0.l && work.x < z0.r + 40.0,
        "a working fish keeps to its reef"
    );
    // A fish turning to need you rises, with a burst of bubbles.
    let bubbles = reef.bubbles.len();
    reef.sync(vec![
        swimmer(1, 0, Mood::Waiting),
        swimmer(2, 0, Mood::Idle),
        swimmer(3, 1, Mood::Done),
    ]);
    assert!(reef.bubbles.len() >= (bubbles + 10).min(320));
    for _ in 0..600 {
        reef.step(1.0 / 60.0);
    }
    assert!(reef.fish[0].y < reef.surface_y + 50.0 * reef.s);
    reef.selected = Some(ResourceId::local(3));
    reef.sync(vec![swimmer(1, 0, Mood::Waiting)]);
    assert_eq!(reef.fish.len(), 1);
    assert_eq!(reef.selected, None, "a gone fish is no longer selected");
}

#[test]
fn crowds_scale_per_reef_and_the_scene_is_deterministic() {
    let fleet: Vec<Swimmer> = (0..12)
        .map(|n| swimmer(n, usize::from(n >= 10), Mood::Working))
        .collect();
    let run = || {
        let mut reef = Reef::default();
        reef.sync(fleet.clone());
        reef.layout(1100.0, 720.0, &hosts(&["mini", "macbook"]));
        reef.advance(0.04);
        reef
    };
    let (a, b) = (run(), run());
    assert_eq!(a.fish[0].k, fish_scale(10));
    assert_eq!(a.fish[11].k, 1.0);
    let at = |r: &Reef| r.fish.iter().map(|f| [f.x, f.y]).collect::<Vec<_>>();
    assert_eq!(at(&a), at(&b));
}

#[test]
fn selecting_a_fish_zooms_to_it_inside_the_scene() {
    let mut reef = Reef::default();
    reef.sync(vec![swimmer(1, 0, Mood::Idle)]);
    reef.layout(1200.0, 780.0, &hosts(&["mini"]));
    assert_eq!(
        reef.camera_target(),
        Cam {
            x: 600.0,
            y: 390.0,
            z: 1.0
        }
    );
    reef.selected = Some(ResourceId::local(1));
    let cam = reef.camera_target();
    assert_eq!(cam.z, 1.85);
    let hh = 780.0 / 3.7;
    assert!(cam.y <= 780.0 - hh + 0.01 && cam.x >= 1200.0 / 3.7 - 0.01);
}
