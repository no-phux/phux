use super::*;

fn dist(a: Pt, b: Pt) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

#[test]
fn systems_clear_the_core_and_each_other_and_stay_in_the_world() {
    let plan = Plan::new(&[8, 1, 0]);
    assert_eq!(plan.core, [plan.side / 2.0, plan.side / 2.0]);
    assert!(
        plan.systems[0].centre[1] < plan.core[1],
        "the first is on top"
    );
    assert_eq!(plan.systems[0].rings(), 2);
    let reach = System::ring_radius(1) + 50.0;
    for (i, system) in plan.systems.iter().enumerate() {
        assert!((dist(system.centre, plan.core) - plan.orbit).abs() < 0.01);
        assert!(dist(system.centre, plan.core) > reach + CORE_R);
        for other in &plan.systems[i + 1..] {
            assert!(dist(system.centre, other.centre) > 2.0 * reach - 1.0);
        }
        for index in 0..system.count {
            for time in [0.0, 17.0, 45.0] {
                let p = system.point(index, time, true);
                assert!(p[0] > 0.0 && p[1] > 0.0 && p[0] < plan.side && p[1] < plan.side);
            }
        }
    }
}

#[test]
fn a_waiting_agent_holds_still_and_agents_on_a_ring_keep_apart() {
    let system = Plan::new(&[6]).systems[0].clone();
    assert_eq!(system.point(0, 0.0, false), system.point(0, 30.0, false));
    assert_ne!(system.point(0, 0.0, true), system.point(0, 30.0, true));
    let spots: Vec<Pt> = (0..6).map(|i| system.point(i, 0.0, false)).collect();
    for (i, a) in spots.iter().enumerate() {
        for b in &spots[i + 1..] {
            assert!(dist(*a, *b) > 90.0);
        }
    }
    let plan = Plan::new(&[3, 3, 3]);
    let scale = plan.fit(1100.0, 720.0);
    assert!((MIN_SCALE..=1.0).contains(&scale) && plan.side * scale <= 720.01);
}

#[test]
fn relations_follow_the_session_or_the_project_and_each_pair_draws_once() {
    let kin = |host, session, project| Kin {
        host,
        session,
        project,
    };
    let agents = [
        kin("mini", "a", Some("phux")),
        kin("mini", "a", Some("phux")),
        kin("mini", "b", None),
        kin("mini", "c", Some("phux")),
        kin("book", "b", None),
    ];
    // 0-1 share both session and project: one line. 1-3 by project only.
    // 2 and 4 share a session name on different machines: unrelated.
    assert_eq!(related(&agents), vec![(0, 1), (1, 3)]);
    assert_eq!(project("/Users/you/src/phux/"), Some("phux"));
    assert_eq!(project("/Users/you"), None);
    assert_eq!(project("/"), None);
}

#[test]
fn a_lone_agent_sits_beside_its_planet_and_every_agent_is_inside_the_bezel() {
    let plan = Plan::new(&[1, 7, 2]);
    // Beside, not on the spoke: off the line from the core through the planet.
    let system = &plan.systems[0];
    let agent = system.point(0, 0.0, false);
    assert!((agent[0] - system.centre[0]).abs() > 90.0, "{agent:?}");
    for system in &plan.systems {
        for index in 0..system.count {
            let p = system.point(index, 12.0, true);
            assert!(dist(p, plan.core) + 30.0 < plan.bezel());
        }
    }
}

#[test]
fn detail_drops_words_then_calm_names_as_the_camera_pulls_back() {
    assert_eq!(Detail::at(1.0), Detail::Full);
    assert_eq!(Detail::at(0.6), Detail::Full);
    assert_eq!(Detail::at(0.5), Detail::Names);
    assert_eq!(Detail::at(0.3), Detail::Urgent);
}
