use super::*;
use crate::reef::CLAUDE;

fn cook(n: u32, host: usize, mood: Mood) -> Cook {
    Cook {
        id: ResourceId::local(n),
        name: format!("agent-{n}"),
        host,
        mood,
        species: &CLAUDE,
        work: None,
    }
}

fn hosts(names: &[&str]) -> Vec<(String, bool)> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| ((*n).to_owned(), i == 0))
        .collect()
}

fn kitchen(w: f32, h: f32, cooks: Vec<Cook>) -> Kitchen {
    let mut k = Kitchen::default();
    k.layout(w, h, &hosts(&["mini", "macbook", "powerhouse-1"]));
    k.sync(cooks);
    k
}

#[test]
fn lines_split_the_width_in_order_and_keep_their_layout() {
    let k = kitchen(1280.0, 800.0, Vec::new());
    let l = &k.lines;
    assert_eq!(l.len(), 3);
    assert!(
        l[0].x1 < l[1].x0 && l[1].x1 < l[2].x0,
        "side by side, in order"
    );
    assert!((l[0].w - l[2].w).abs() < 1e-3);
    assert!(
        l.iter().all(|l| l.island.is_some()),
        "a tall line gets an island"
    );
    // The same host makes the same line wherever it sits.
    let alone = line(
        "macbook",
        false,
        [0, 1],
        [110.0, 696.0, 1280.0],
        false,
        k.cs,
    );
    // Its tickets come off its own rng: wider, it just has more of them.
    assert_eq!(alone.tickets[..4], l[1].tickets[..4]);
    // Portrait stacks them instead.
    let p = kitchen(390.0, 844.0, Vec::new());
    assert!(p.portrait && p.lines[0].y1 < p.lines[1].y0);
    assert!(p.lines.iter().all(|l| l.island.is_none()));
}

#[test]
fn moods_send_chefs_to_their_spots() {
    let l = &kitchen(1280.0, 800.0, Vec::new()).lines[0];
    let goals = plan(
        l,
        &[
            (Mood::Waiting, 0),
            (Mood::Waiting, 0),
            (Mood::Done, 0),
            (Mood::Working, 3),
            (Mood::Working, 3),
            (Mood::Idle, 0),
        ],
    );
    let last = l.pass.len() - 1;
    assert_eq!(goals[..3], [Goal::Pass(0), Goal::Pass(1), Goal::Pass(last)]);
    assert_eq!(goals[3], Goal::Station(3));
    assert_eq!(
        goals[4],
        Goal::Station(4),
        "a taken station sends the next chef along"
    );
    assert_eq!(goals[5], Goal::Break(0));
    // Every station taken: the rest share, nobody is lost.
    let crowd: Vec<(Mood, u32)> = (0..l.work.len() as u32 + 3)
        .map(|n| (Mood::Working, n))
        .collect();
    assert_eq!(plan(l, &crowd).len(), crowd.len());
}

#[test]
fn chefs_walk_round_the_island_and_the_pass() {
    let l = &kitchen(1280.0, 800.0, Vec::new()).lines[0];
    let is = l.island.unwrap();
    // From the pass to a station on the back counter, behind the island.
    let back = l
        .work
        .iter()
        .position(|s| s.iy < l.cy1 && s.x > is.x0 && s.x < is.x1)
        .unwrap();
    let (from, to) = (l.at(Goal::Pass(2)), l.at(Goal::Station(back)));
    assert!(
        l.blocks[1].cuts(from, to),
        "the straight way cuts the island"
    );
    let way = route(from, to, &l.blocks);
    assert!(way.len() > 1 && *way.last().unwrap() == to);
    let mut at = from;
    for &p in &way {
        assert!(l.blocks.iter().all(|b| !b.cuts(at, p)), "{at:?} to {p:?}");
        at = p;
    }
    // From the break corner to the pass, round the shelf's end.
    let way = route(l.at(Goal::Break(0)), l.at(Goal::Pass(3)), &l.blocks);
    let mut at = l.at(Goal::Break(0));
    for &p in &way {
        assert!(l.blocks.iter().all(|b| !b.cuts(at, p)));
        at = p;
    }
    // A clear way is just the goal.
    assert_eq!(
        route(l.at(Goal::Pass(0)), l.at(Goal::Pass(1)), &l.blocks),
        vec![l.at(Goal::Pass(1))]
    );
}

#[test]
fn a_chef_walks_to_its_new_spot_and_stays_in_its_line() {
    let mut k = kitchen(
        1280.0,
        800.0,
        vec![cook(1, 1, Mood::Idle), cook(2, 1, Mood::Working)],
    );
    assert!(
        k.chefs.iter().all(|c| c.arrived()),
        "new chefs start at their spots"
    );
    let start = [k.chefs[0].x, k.chefs[0].y];
    k.sync(vec![cook(1, 1, Mood::Waiting), cook(2, 1, Mood::Working)]);
    assert!(!k.chefs[0].arrived());
    assert!(k.chefs[0].ring0 > k.t, "the bell starts as it arrives");
    for _ in 0..600 {
        k.step(sim::STEP);
        let c = &k.chefs[0];
        let l = &k.lines[1];
        assert!(c.x >= l.x0 - 20.0 && c.x <= l.x1 + 20.0);
    }
    let c = &k.chefs[0];
    assert!(c.arrived() && [c.x, c.y] != start);
    assert_eq!(c.goal, Goal::Pass(0));
    assert_eq!([c.x, c.y], k.lines[1].at(Goal::Pass(0)));
    // Done: a mint burst where it stands.
    k.sync(vec![cook(1, 1, Mood::Done), cook(2, 1, Mood::Working)]);
    assert!(k.parts.iter().filter(|p| p.kind == PartKind::Mint).count() >= 12);
}

#[test]
fn the_toque_grows_with_rank_and_absent_stats_are_a_cook() {
    let mut k = kitchen(1280.0, 800.0, vec![cook(1, 0, Mood::Working)]);
    assert_eq!(k.chefs[0].tier, 1);
    assert_eq!(RANK[k.chefs[0].tier], "Cook");
    let mut star = cook(1, 0, Mood::Working);
    star.work = Some(Work {
        turns: 40,
        working_s: 0.0,
    });
    k.sync(vec![star]);
    assert_eq!(k.chefs[0].tier, 4);
    assert!(HAT.windows(2).all(|w| w[0] < w[1]));
    let (lo, hi) = (k.cs * 0.9, k.chef_scale(&k.chefs[0]));
    assert!(hi > lo);
}

#[test]
fn the_scene_is_deterministic() {
    let run = || {
        let mut k = kitchen(
            1280.0,
            800.0,
            (0..12)
                .map(|n| cook(n, n as usize % 3, Mood::Working))
                .collect(),
        );
        k.nt_target = 1.0;
        for _ in 0..300 {
            k.step(sim::STEP);
        }
        k.parts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()
    };
    let a = run();
    assert!(!a.is_empty());
    assert_eq!(a, run());
}
