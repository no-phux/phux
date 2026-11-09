use phux_protocol::ids::ResourceId;

use super::sim::STEP;
use super::*;
use crate::creature::Work;
use crate::reef::{CLAUDE, CODEX};

fn hosts(names: &[&str]) -> Vec<(String, bool)> {
    names.iter().map(|n| (n.to_string(), true)).collect()
}

fn agent(id: u32, host: usize, mood: Mood) -> Agent {
    Agent {
        id: ResourceId::local(id),
        name: format!("agent-{id}"),
        host,
        mood,
        species: if id.is_multiple_of(2) {
            &CLAUDE
        } else {
            &CODEX
        },
        number: id,
        work: None,
    }
}

fn pitch(agents: Vec<Agent>) -> Pitch {
    let mut p = Pitch::default();
    p.sync(agents);
    p.layout(1280.0, 800.0, &hosts(&["mini", "macbook", "powerhouse-1"]));
    p
}

fn run(p: &mut Pitch, secs: f32) {
    for _ in 0..(secs / STEP) as usize {
        p.step(STEP);
    }
}

#[test]
fn machines_split_the_pitch_in_order_with_their_own_cones() {
    let p = pitch(Vec::new());
    assert!(!p.portrait);
    assert_eq!(p.zones.len(), 3);
    let names: Vec<&str> = p.zones.iter().map(|z| z.host.as_str()).collect();
    assert_eq!(names, ["mini", "macbook", "powerhouse-1"]);
    for (i, z) in p.zones.iter().enumerate() {
        assert!((z.zw - p.lp / 3.0).abs() < 1e-3);
        assert!((z.z0 - i as f32 * z.zw).abs() < 1e-3);
        assert!(z.cones.iter().all(|c| c.l > z.z0 && c.l < z.z0 + z.zw));
    }
    // The ground fits inside the view's chrome, and a host seeds the same cones.
    assert!(p.g.y >= INSET[0] && p.g.y + p.g.h <= 800.0 - INSET[1]);
    let again = pitch(Vec::new());
    assert_eq!(again.zones[1].cones[0].l, p.zones[1].cones[0].l);
}

#[test]
fn a_tall_view_turns_the_pitch_upright() {
    let mut p = Pitch::default();
    p.layout(390.0, 844.0, &hosts(&["mini", "macbook"]));
    assert!(p.portrait);
    assert_eq!(p.lp, p.p.h);
    assert_eq!(p.to_s(10.0, 3.0), [p.p.x + 3.0, p.p.y + 10.0]);
}

#[test]
fn waiting_players_stand_on_the_touchline_and_idle_ones_in_the_dugout() {
    let p = pitch(Vec::new());
    let z = &p.zones[1];
    let wait: Vec<Pt> = (0..3)
        .map(|i| post(Mood::Waiting, z, [i, 3], p.k, p.strip).unwrap())
        .collect();
    let idle = post(Mood::Idle, z, [0, 1], p.k, p.strip).unwrap();
    for w in &wait {
        assert!(
            w[1] < 0.0 && w[1] > idle[1],
            "beyond the line, short of the dugout"
        );
        assert!(w[0] > z.z0 && w[0] < z.z0 + z.zw);
    }
    assert!(
        wait[0][0] < wait[1][0] && wait[1][0] < wait[2][0],
        "slots spread along"
    );
    assert!(
        idle[0] < wait[0][0],
        "the dugout is upfield of the touchline post"
    );
    assert_eq!(post(Mood::Working, z, [0, 1], p.k, p.strip), None);
}

#[test]
fn tiers_dress_the_kit_from_rookie_to_legend() {
    assert_eq!(kit(0).boots, Boots::Black);
    assert_eq!(
        kit(1),
        Kit {
            boots: Boots::Sleeve,
            armband: false,
            trim: false,
            star: false
        }
    );
    assert!(kit(2).armband && !kit(2).trim);
    assert_eq!(
        kit(3),
        Kit {
            boots: Boots::Gold,
            armband: true,
            trim: true,
            star: false
        }
    );
    assert!(kit(4).star);
    // No stats is tier 1, a regular.
    let p = pitch(vec![agent(1, 0, Mood::Working)]);
    assert_eq!(p.players[0].tier, 1);
    let mut vet = agent(2, 0, Mood::Working);
    vet.work = Some(Work {
        turns: 40,
        working_s: 0.0,
    });
    let p = pitch(vec![vet]);
    assert_eq!(RANKS[p.players[0].tier], "Legend");
}

#[test]
fn players_settle_where_their_mood_puts_them() {
    let mut p = pitch(vec![
        agent(1, 0, Mood::Working),
        agent(2, 1, Mood::Waiting),
        agent(3, 2, Mood::Idle),
        agent(4, 2, Mood::Done),
    ]);
    run(&mut p, 6.0);
    let at = |id: u32| {
        p.players
            .iter()
            .find(|q| q.who.id == ResourceId::local(id))
            .unwrap()
    };
    let (work, wait, idle, done) = (at(1), at(2), at(3), at(4));
    let z = |i: usize| &p.zones[i];
    assert!(work.l > z(0).z0 && work.l < z(0).z0 + z(0).zw && work.s > 0.0 && work.s < p.sp);
    assert!(wait.s < 0.0 && wait.l > z(1).z0 && wait.l < z(1).z0 + z(1).zw);
    assert!(idle.s < wait.s && idle.sit > 0.9, "seated in the dugout");
    assert!(
        done.s > 0.0 && done.l > z(2).z0,
        "cooling down in its own third"
    );
    assert!(
        p.parts.is_empty(),
        "already done when it arrived: no celebration"
    );
}

#[test]
fn finishing_celebrates_once_with_confetti() {
    let mut p = pitch(vec![agent(1, 0, Mood::Working)]);
    run(&mut p, 1.0);
    p.sync(vec![agent(1, 0, Mood::Done)]);
    p.step(STEP);
    assert!(p.players[0].cel > 2.5);
    assert_eq!(p.parts.len(), 28);
    run(&mut p, 3.0);
    assert!(p.players[0].cel <= 0.0 && p.parts.is_empty());
}

#[test]
fn a_squad_passes_its_ball_around() {
    let mut p = pitch((1..=3).map(|i| agent(i, 1, Mood::Working)).collect());
    let mut owners = std::collections::HashSet::new();
    for _ in 0..(20.0 / STEP) as usize {
        p.step(STEP);
        if let Some(o) = &p.zones[1].ball.owner {
            owners.insert(o.clone());
        }
    }
    assert!(owners.len() >= 2, "the ball changed feet: {owners:?}");
    // Other zones' balls sit still with nobody to play them.
    assert!(p.zones[0].ball.owner.is_none() && p.zones[0].ball.fl.is_none());
}

#[test]
fn the_same_fleet_plays_out_the_same() {
    let fleet = || {
        (1..=6)
            .map(|i| agent(i, (i % 3) as usize, Mood::Working))
            .collect()
    };
    let (mut a, mut b) = (pitch(fleet()), pitch(fleet()));
    run(&mut a, 3.0);
    run(&mut b, 3.0);
    for (x, y) in a.players.iter().zip(&b.players) {
        assert_eq!((x.l, x.s), (y.l, y.s));
    }
}

#[test]
fn sync_keeps_players_and_drops_a_gone_selection() {
    let mut p = pitch(vec![agent(1, 0, Mood::Working), agent(2, 0, Mood::Working)]);
    run(&mut p, 1.0);
    let before = p.players[1].l;
    p.selected = Some(ResourceId::local(1));
    p.sync(vec![agent(2, 0, Mood::Working)]);
    assert_eq!(p.players.len(), 1);
    assert_eq!(p.players[0].l, before, "kept its place");
    assert_eq!(p.selected, None);
}

#[test]
fn the_background_is_painted_opaque_at_twice_the_size() {
    let p = pitch(Vec::new());
    for night in [false, true] {
        let (w, h, rgba) = ground::render(&p, night).unwrap();
        assert_eq!((w, h), (2560, 1600));
        assert!(rgba.chunks(4).step_by(997).all(|px| px[3] == 255));
    }
}
