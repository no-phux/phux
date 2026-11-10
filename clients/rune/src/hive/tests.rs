use super::*;
use crate::reef::{CLAUDE, CODEX};

fn forager(n: u32, host: usize, mood: Mood) -> Forager {
    Forager {
        id: ResourceId::local(n),
        name: format!("agent-{n}"),
        host,
        mood,
        species: if n.is_multiple_of(2) { &CLAUDE } else { &CODEX },
        tier: 1,
        work: None,
    }
}

fn hosts(names: &[&str]) -> Vec<(String, bool)> {
    names.iter().map(|n| ((*n).to_owned(), true)).collect()
}

fn hive(foragers: Vec<Forager>) -> Hive {
    let mut h = Hive::default();
    h.sync(foragers);
    h.layout(1280.0, 800.0, &hosts(&["mini", "macbook", "powerhouse-1"]));
    h
}

#[test]
fn frames_stand_side_by_side_wide_and_stack_tall() {
    let wide = frame_rects(3, [310.0, 1256.0], [48.0, 740.0], false, 500.0);
    assert_eq!(wide.len(), 3);
    assert!(
        wide.iter()
            .all(|f| f[1] == 48.0 && (f[3] - 692.0).abs() < 1e-3)
    );
    assert!(wide[0][0] < wide[1][0] && wide[1][0] < wide[2][0]);
    assert!((wide[2][0] + wide[2][2] - 1256.0).abs() < 1e-3);
    let tall = frame_rects(3, [88.0, 378.0], [48.0, 784.0], true, 500.0);
    assert!(tall.iter().all(|f| f[0] == 88.0));
    assert!(tall[0][1] < tall[1][1] && tall[1][1] < tall[2][1]);
    // Too many to fit one row: they wrap, every one inside the band.
    let many = frame_rects(9, [310.0, 1256.0], [48.0, 740.0], false, 500.0);
    assert!(
        many.iter()
            .all(|f| f[2] >= MIN_W && f[0] + f[2] <= 1256.0 + 1e-3)
    );
    assert!(many.iter().all(|f| f[1] + f[3] <= 740.0 + 1e-3));
    assert!(frame_rects(0, [0.0, 1.0], [0.0, 1.0], false, 500.0).is_empty());
    // One machine: a frame no wider than the cap, centred.
    let one = frame_rects(1, [310.0, 1256.0], [48.0, 740.0], false, 400.0);
    assert_eq!(one[0][2], 400.0);
    assert!((one[0][0] - (310.0 + (946.0 - 400.0) / 2.0)).abs() < 1e-3);
}

#[test]
fn the_comb_is_the_same_from_the_same_host_name() {
    let (a, b) = (hive(Vec::new()), hive(Vec::new()));
    for (fa, fb) in a.frames.iter().zip(&b.frames) {
        assert!(!fa.cells.is_empty());
        let kinds = |f: &Frame| f.cells.iter().map(|c| c.kind).collect::<Vec<_>>();
        assert_eq!(kinds(fa), kinds(fb));
    }
    let top = &a.frames[0];
    let honey_top = top
        .cells
        .iter()
        .filter(|c| (c.at[1] - top.iy) / top.ih < 0.3)
        .filter(|c| c.kind == Kind::Honey)
        .count();
    let all_top = top
        .cells
        .iter()
        .filter(|c| (c.at[1] - top.iy) / top.ih < 0.3)
        .count();
    assert!(honey_top * 2 > all_top, "honey fills the top of the comb");
}

#[test]
fn every_bee_gets_its_own_cell_in_the_middle_of_its_frame() {
    let h = hive(
        (0..12)
            .map(|n| forager(n, n as usize % 3, Mood::Working))
            .collect(),
    );
    let mut seen = std::collections::HashSet::new();
    for b in &h.bees {
        let f = h.frame_of(b);
        let c = &f.cells[b.home];
        assert!(c.home);
        let v = (c.at[1] - f.iy) / f.ih;
        assert!(v > 0.3 && v < 0.78, "{v}");
        assert!(seen.insert((b.who.host, b.home)), "shared cell");
    }
}

#[test]
fn moods_act_out_round_their_cells() {
    let mut h = hive(vec![
        forager(1, 0, Mood::Waiting),
        forager(2, 1, Mood::Done),
        forager(3, 2, Mood::Idle),
        forager(4, 0, Mood::Working),
    ]);
    for _ in 0..600 {
        h.step(STEP);
    }
    let by = |n: u32| {
        h.bees
            .iter()
            .find(|b| b.who.id == ResourceId::local(n))
            .unwrap()
    };
    let near = |b: &Bee, p: Pt, d: f32| (b.x - p[0]).hypot(b.y - p[1]) < d;
    let (w, d, i) = (by(1), by(2), by(3));
    // Dancing over its cell, with a long amber figure behind it.
    assert!(near(w, h.home_of(w), w.len(h.s) * 3.0));
    assert!(w.trail.len() > 26);
    // Hovering over its capped cell.
    assert!(near(d, h.home_of(d), d.len(h.s)));
    assert!((d.cap - 1.0).abs() < 1e-6);
    // Asleep in the cluster.
    assert!(near(i, h.frame_of(i).idle, i.len(h.s)));
    assert!((i.ang - i.rest_ang).abs() < 0.05);
    // Working: has been out to the meadow and back with pollen.
    let mut out = false;
    for _ in 0..1200 {
        h.step(STEP);
        out |= h.bees[3].x < h.mw;
    }
    assert!(out, "a working bee forages in the meadow");
}

#[test]
fn turning_to_need_you_bursts_and_starts_a_fresh_trail() {
    let mut h = hive(vec![forager(1, 0, Mood::Working)]);
    for _ in 0..60 {
        h.step(STEP);
    }
    h.sync(vec![forager(1, 0, Mood::Waiting)]);
    assert_eq!(h.bees[0].burst, 1.0);
    assert!(h.bees[0].trail.is_empty());
    // Gone: its cell is free again and it's no longer selected.
    h.selected = Some(ResourceId::local(1));
    let home = h.bees[0].home;
    h.sync(Vec::new());
    assert!(!h.frames[0].cells[home].home);
    assert_eq!(h.selected, None);
}

#[test]
fn tiers_add_their_flourishes() {
    assert!(!flourish(1).tips && !flourish(1).crown);
    assert!(flourish(2).tips && flourish(2).eye < flourish(3).eye);
    let queen = flourish(4);
    assert!(queen.crown && queen.abdomen > 1.0);
    let mut b = hive(vec![forager(1, 0, Mood::Idle)]).bees.remove(0);
    let base = b.len(1.0);
    b.who.tier = 4;
    assert!(b.len(1.0) > base * 1.3);
}

#[test]
fn the_sim_is_deterministic_and_idle_bees_draw_first() {
    let make = || {
        let mut h = hive(vec![
            forager(1, 0, Mood::Working),
            forager(2, 0, Mood::Idle),
            forager(3, 1, Mood::Waiting),
        ]);
        for _ in 0..300 {
            h.step(STEP);
        }
        h.bees.iter().map(|b| (b.x, b.y)).collect::<Vec<_>>()
    };
    assert_eq!(make(), make());
    let h = hive(vec![
        forager(1, 0, Mood::Working),
        forager(2, 0, Mood::Idle),
    ]);
    assert_eq!(h.draw_order()[0].who.mood, Mood::Idle);
}

#[test]
fn the_camera_follows_the_selected_bee_and_stays_inside() {
    let mut h = hive(vec![forager(1, 0, Mood::Idle)]);
    assert_eq!(h.camera_target().z, 1.0);
    h.selected = Some(ResourceId::local(1));
    let to = h.camera_target();
    assert!(to.z > 1.5);
    let (hw, hh) = (h.w / (2.0 * to.z), h.h / (2.0 * to.z));
    assert!(to.x >= hw && to.x <= h.w - hw && to.y >= hh && to.y <= h.h - hh);
}

#[test]
fn pills_stack_upward_instead_of_overlapping() {
    let placed = place_pills(
        &[
            ([200.0, 300.0], 10),
            ([210.0, 300.0], 12),
            ([900.0, 300.0], 8),
        ],
        1280.0,
    );
    assert_eq!(placed[0][1], 300.0);
    assert_eq!(placed[1][1], 300.0 - 22.0, "moved up past the first");
    assert_eq!(placed[2][1], 300.0, "far away: stays");
    // Kept inside the view.
    let edge = place_pills(&[([2.0, 50.0], 20)], 1280.0);
    assert_eq!(edge[0][0], 6.0);
}
