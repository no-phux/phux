use super::*;
use crate::app::fleet::Destination;
use crate::connection::AgentAttention;

fn agent(host: Option<&str>, terminal: u32, kind: &str, project: &str, state: AgentState) -> Agent {
    Agent {
        destination: Destination {
            host: host.map(str::to_owned),
            authority: None,
            key: None,
            name: format!("s{terminal}"),
        },
        terminal: phux_protocol::ResourceId::local(terminal),
        boot: None,
        info: AgentInfo {
            name: format!("a{terminal}"),
            kind: Some(kind.into()),
            state,
            attention: AgentAttention::None,
            changed_at_ms: 0,
        },
        title: String::new(),
        project: Some(project.into()),
        stats: None,
    }
}

#[test]
fn harnesses_map_to_their_species_and_the_rest_share_one() {
    assert_eq!(species(Harness::Claude).pattern, Pattern::Bands);
    assert_eq!(species(Harness::Claude).body, [255., 118., 58.]);
    assert_eq!(species(Harness::Codex).pattern, Pattern::Mask);
    assert_eq!(species(Harness::OpenCode).tall, 1.7);
    assert_eq!(species(Harness::Gemini).fin, [104., 222., 255.]);
    let other = species(Harness::Other);
    assert_eq!(other.pattern, Pattern::Plain);
    for harness in [Harness::Fx, Harness::Pi, Harness::Grok, Harness::Aider] {
        assert_eq!(species(harness), other);
    }
}

#[test]
fn moods_follow_the_galaxy() {
    let mut info = agent(None, 1, "claude", "p", AgentState::Idle).info;
    assert_eq!(Mood::of(&info), Mood::Idle);
    info.attention = AgentAttention::High;
    assert_eq!(Mood::of(&info), Mood::Waiting);
    info.attention = AgentAttention::None;
    for (state, mood) in [
        (AgentState::Blocked, Mood::Waiting),
        (AgentState::Done, Mood::Done),
        (AgentState::Working, Mood::Working),
        (AgentState::Unknown, Mood::Idle),
    ] {
        info.state = state;
        assert_eq!(Mood::of(&info), mood);
    }
}

#[test]
fn crowded_reefs_shrink_their_fish_to_a_floor() {
    assert_eq!(crowd(1), 1.);
    assert_eq!(crowd(8), 1.);
    assert!((crowd(16) - 0.5f32.sqrt()).abs() < 1e-6);
    assert_eq!(crowd(100), 0.55);
}

#[test]
fn zones_split_the_width_and_grow_the_same_coral_each_launch() {
    let hosts = [None, Some("mini".to_owned()), Some("nas".to_owned())];
    let a = Sim::new(1200., 780., &hosts, &[], false);
    let b = Sim::new(1200., 780., &hosts, &[], true);
    assert_eq!(a.zones.len(), 3);
    for (i, z) in a.zones.iter().enumerate() {
        assert!((z.l - 400. * i as f32).abs() < 0.01 && (z.r - z.l - 400.).abs() < 0.01);
        assert!((z.cx - (z.l + 200.)).abs() < 0.01);
    }
    assert_eq!(a.zones[0].name, "Local");
    assert_eq!(a.zones[1].corals[0].x, b.zones[1].corals[0].x);
    assert_eq!(a.zones[1].corals.len(), 4);
    assert_eq!(hash("a"), 0xE40C_292C, "FNV-1a");
}

#[test]
fn the_canopy_tops_every_coral() {
    let hosts = [None, Some("mini".to_owned()), Some("nas".to_owned())];
    let sim = Sim::new(1200., 780., &hosts, &[], false);
    let (s, eps) = (sim.s, 1e-3);
    for z in &sim.zones {
        assert_eq!(z.home_y, z.canopy, "the buoy rope ends on the canopy");
        for c in &z.corals {
            assert!(z.canopy <= sim.sand_y(c.x) + 3. * s - c.hmax + eps);
        }
        for f in &z.fans {
            assert!(z.canopy <= sim.sand_y(f.x) + 2. * s - f.h + eps);
        }
        for a in &z.anemones {
            assert!(z.canopy <= sim.sand_y(a.x) + 3. * s - a.r * 0.9 - a.len + eps);
        }
        for b in &z.brains {
            assert!(z.canopy <= sim.sand_y(b.x) + 4. * s - b.r * 0.78 + eps);
        }
        let (top, bottom) = sim.band(z);
        assert!(
            top < bottom && bottom < z.canopy,
            "working band above the coral"
        );
    }
}

#[test]
fn moods_pick_their_places_over_their_own_reef() {
    let hosts = [None, Some("nas".to_owned())];
    let agents = [
        agent(Some("nas"), 1, "claude", "p", AgentState::Blocked),
        agent(Some("nas"), 2, "codex", "q", AgentState::Done),
        agent(Some("nas"), 3, "gemini", "r", AgentState::Idle),
        agent(Some("nas"), 4, "codex", "s", AgentState::Done),
        agent(Some("nas"), 5, "gemini", "t", AgentState::Idle),
    ];
    let sim = Sim::new(1200., 780., &hosts, &agents, false);
    let zone = &sim.zones[1];
    let [waiting, done, idle, done2, idle2] = [0, 1, 2, 3, 4].map(|i| sim.target(&sim.fish[i]));
    for (x, _) in [waiting, done, idle, done2, idle2] {
        assert!(x > zone.l && x < zone.r, "over its reef");
    }
    assert!(
        waiting.1 < sim.surface_y + 40. * sim.s,
        "just under the surface"
    );
    for (_, y) in [done, done2] {
        assert!(
            y < zone.canopy - 40. * sim.s && y > waiting.1,
            "above the reef"
        );
    }
    for (_, y) in [idle, idle2] {
        assert!(y < zone.canopy && y > done.1, "resting on the reef");
    }
    // Two of a mood spread across the reef, not stacked.
    assert!((done2.0 - done.0).abs() > zone.w * 0.25);
    assert!((idle2.0 - idle.0).abs() > zone.w * 0.25);
    // Settled: the waiting fish has risen to the surface.
    assert!((sim.fish[0].y - waiting.1).abs() < 30.);
}

#[test]
fn schools_prefer_their_own_depths() {
    let a = agent(Some("nas"), 1, "claude", "phux", AgentState::Working);
    let b = agent(Some("nas"), 2, "claude", "docs", AgentState::Working);
    let mate = agent(Some("nas"), 3, "codex", "phux", AgentState::Working);
    assert_eq!(lane(&a), lane(&mate), "one school, one depth");
    assert!((lane(&a) - lane(&b)).abs() > 0.1, "schools differ");
    for l in [lane(&a), lane(&b)] {
        assert!((0. ..1.).contains(&l));
    }
    // The same lanes as every other platform's reef.
    for (host, project, want) in [
        ("mini", "phux", 0.817),
        ("mini", "website", 0.877),
        ("macbook", "phux-mobile", 0.524),
    ] {
        let got = lane(&agent(
            Some(host),
            4,
            "claude",
            project,
            AgentState::Working,
        ));
        assert!((got - want).abs() < 5e-4, "{host}/{project}: {got}");
    }
}

#[test]
fn a_school_is_the_related_agents_and_fish_shrink_on_a_crowded_reef() {
    let hosts = [None];
    let mut agents: Vec<_> = (0..10)
        .map(|i| agent(None, i, "claude", "solo", AgentState::Working))
        .collect();
    agents[0].project = Some("phux".into());
    agents[1].project = Some("phux".into());
    for (i, a) in agents.iter_mut().enumerate().skip(2) {
        a.project = Some(format!("p{i}"));
    }
    let sim = Sim::new(1200., 780., &hosts, &agents, false);
    assert_eq!(sim.fish[0].mates, vec![1]);
    assert!(sim.fish[5].mates.is_empty());
    let len = sim.fish[0].len(sim.s);
    assert!((len - sim.fish[0].lb * sim.s * crowd(10)).abs() < 1e-4);
    assert!(len < 42. * sim.s);
}

#[test]
fn a_long_swim_stays_in_the_water_and_follows_mood_changes() {
    let hosts = [None, Some("nas".to_owned())];
    let mut agents: Vec<_> = (0..12)
        .map(|i| agent(Some("nas"), i, "codex", "p", AgentState::Working))
        .collect();
    let mut sim = Sim::new(900., 600., &hosts, &agents, true);
    for _ in 0..600 {
        sim.step(1. / 30.);
    }
    agents[3].info.state = AgentState::Blocked;
    sim.sync(900., 600., &hosts, &agents, false);
    assert!(!sim.bubbles.is_empty(), "a burst on turning to waiting");
    for _ in 0..300 {
        sim.step(1. / 30.);
    }
    let zone = &sim.zones[1];
    for f in &sim.fish {
        assert!(f.x.is_finite() && f.y.is_finite());
        assert!(f.x > zone.l - 60. && f.x < zone.r + 60., "{}", f.x);
        assert!(
            f.y > sim.surface_y && f.y < zone.canopy,
            "above the coral {}",
            f.y
        );
    }
    assert!(sim.fish[3].y < sim.surface_y + 60. * sim.s, "risen");
    assert!(sim.nt < 0.01, "the day came back");
}

#[test]
fn waiting_fish_spread_across_the_reef_and_stagger_when_crowded() {
    // Two on a wide reef: one row, spread over the middle 84 percent.
    let (gap, row) = waiting_slot(600., 1, 2, 1.);
    assert!((gap - 252.).abs() < 1e-3 && row == 0.);
    // Seven on the same reef: tags would touch, so alternate fish drop a row.
    let rows = (0..7)
        .map(|i| waiting_slot(600., i, 7, 1.).1)
        .collect::<Vec<_>>();
    assert_eq!(rows, [0., 1., 0., 1., 0., 1., 0.]);
    assert!(waiting_slot(600., 0, 7, 1.).0 < 96.);
    // Settled: every one over its reef and none sharing a spot.
    let hosts = [None];
    let agents: Vec<_> = (0..7)
        .map(|i| agent(None, i, "claude", "p", AgentState::Blocked))
        .collect();
    let sim = Sim::new(700., 780., &hosts, &agents, false);
    let spots = sim.fish.iter().map(|f| sim.target(f)).collect::<Vec<_>>();
    let zone = &sim.zones[0];
    for (i, a) in spots.iter().enumerate() {
        assert!(a.0 > zone.l + zone.w * 0.08 && a.0 < zone.r - zone.w * 0.08);
        for b in &spots[i + 1..] {
            assert!((a.0 - b.0).abs() > 40. || (a.1 - b.1).abs() > 40. * sim.s);
        }
    }
}

#[test]
fn fish_grow_with_work_and_level_up_only_on_a_rise() {
    use crate::connection::stats::AgentStats;
    let tally = |turns| {
        Some(AgentStats {
            turns,
            working_s: 0,
            at_ms: 0,
        })
    };
    let hosts = [None];
    let mut agents = vec![agent(None, 1, "claude", "p", AgentState::Idle)];
    let mut sim = Sim::new(1200., 780., &hosts, &agents, false);
    assert_eq!(sim.fish[0].tier, 1, "no tally: a reef fish");
    let plain = sim.fish[0].len(sim.s);
    agents[0].stats = tally(40);
    sim.sync(1200., 780., &hosts, &agents, false);
    assert_eq!(sim.fish[0].tier, 4);
    assert_eq!(sim.fish[0].level_up, 1., "rose while watching");
    assert!((sim.fish[0].len(sim.s) - plain * 1.8).abs() < 1e-3);
    assert!(sim.bubbles.len() >= 14, "a burst of bubbles");
    for _ in 0..90 {
        sim.step(1. / 30.);
    }
    assert!(sim.fish[0].level_up < 0.01, "faded in about 2.2 seconds");
    assert!(!sim.motes.is_empty(), "a legend trails motes");
    // A host restart resets the tally: a quiet regrade.
    agents[0].stats = tally(0);
    sim.sync(1200., 780., &hosts, &agents, false);
    assert_eq!((sim.fish[0].tier, sim.fish[0].level_up), (0, 0.));
    // A fish first seen already grown does not flash.
    agents.push(agent(None, 2, "codex", "p", AgentState::Idle));
    agents[1].stats = tally(12);
    sim.sync(1200., 780., &hosts, &agents, false);
    assert_eq!((sim.fish[1].tier, sim.fish[1].level_up), (3, 0.));
}
