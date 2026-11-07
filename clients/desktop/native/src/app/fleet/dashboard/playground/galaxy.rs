//! The galaxy's geometry: the fleet as a core, each machine a planet on one
//! orbit round it, and each agent on a ring round its machine (the mobile
//! apps' GalaxyPlan, the same numbers). Only working agents orbit, and slowly:
//! one that needs you holds still, so it is easy to click. Also which agents
//! are related, which every map that draws links reads.

use std::f32::consts::{PI, TAU};

use super::plan::Pt;
use crate::app::fleet::Agent;

/// The first ring's radius: a ghost's label is about 100 points wide.
pub(crate) const SHELL: f32 = 100.;
pub(crate) const RING_GAP: f32 = 95.;
pub(crate) const PER_RING: usize = 6;
/// The core's reach, rings included.
pub(crate) const CORE_R: f32 = 70.;
const PADDING: f32 = 70.;
/// Seconds a working agent takes to go once round.
const PERIOD: f32 = 90.;

/// One machine's planet and its agents.
pub(crate) struct System {
    pub host: Option<String>,
    pub online: bool,
    pub agents: Vec<Agent>,
    pub centre: Pt,
    /// The planet's angle about the core, radians.
    pub angle: f32,
}

impl System {
    pub fn rings(&self) -> usize {
        self.agents.len().div_ceil(PER_RING).max(1)
    }

    /// Agent `index` at `time`; only a `roaming` agent moves round.
    pub fn point(&self, index: usize, time: f32, roaming: bool) -> Pt {
        let ring = index / PER_RING;
        let on_ring = PER_RING.min(self.agents.len() - ring * PER_RING);
        let step = TAU / on_ring as f32;
        // A quarter turn off the planet's own spoke keeps the first agent
        // beside the planet, clear of its name; alternate rings sit half a
        // step round, so agents never line up.
        let mut angle =
            self.angle + PI / 2. + step * ((index % PER_RING) as f32 + (ring % 2) as f32 / 2.);
        if roaming {
            angle += time * TAU / PERIOD;
        }
        let radius = SHELL + RING_GAP * ring as f32;
        Pt {
            x: self.centre.x + radius * angle.cos(),
            y: self.centre.y + radius * angle.sin(),
        }
    }
}

pub(crate) struct Galaxy {
    pub systems: Vec<System>,
    pub core: Pt,
    /// From the core to every planet.
    pub orbit: f32,
    /// The world is `side` by `side`, origin zero.
    pub side: f32,
}

impl Galaxy {
    /// Machines that need you first and offline ones last, the first on top
    /// and the rest clockwise. Agents go to their machine in catalog order.
    pub fn new(hosts: Vec<(Option<String>, bool)>, agents: &[Agent]) -> Galaxy {
        let mut systems: Vec<(Option<String>, bool, Vec<Agent>)> = hosts
            .into_iter()
            .map(|(host, online)| {
                let mine = agents
                    .iter()
                    .filter(|agent| agent.destination.host == host)
                    .cloned()
                    .collect();
                (host, online, mine)
            })
            .collect();
        systems.sort_by_key(|(_, online, agents)| {
            if agents.iter().any(|agent| agent.info.needs_you()) {
                0
            } else if *online {
                1
            } else {
                2
            }
        });
        let reach = systems
            .iter()
            .map(|(_, _, agents)| {
                SHELL + RING_GAP * (agents.len().saturating_sub(1) / PER_RING) as f32 + 50.
            })
            .fold(0., f32::max);
        let count = systems.len();
        // Clear of the core, and neighbours clear of each other round the orbit.
        let around = if count > 1 {
            (reach + 20.) / (PI / count as f32).sin()
        } else {
            0.
        };
        let orbit = (reach + CORE_R + 20.).max(around);
        let half = if count == 0 {
            160.
        } else {
            orbit + reach + PADDING
        };
        let core = Pt { x: half, y: half };
        let systems = systems
            .into_iter()
            .enumerate()
            .map(|(index, (host, online, agents))| {
                let angle = -PI / 2. + TAU * index as f32 / count as f32;
                System {
                    host,
                    online,
                    agents,
                    centre: Pt {
                        x: core.x + orbit * angle.cos(),
                        y: core.y + orbit * angle.sin(),
                    },
                    angle,
                }
            })
            .collect();
        Galaxy {
            systems,
            core,
            orbit,
            side: 2. * half,
        }
    }

    /// The scale that shows the whole galaxy in a `width` by `height` scene.
    pub fn fit(&self, width: f32, height: f32) -> f32 {
        if width <= 0. || height <= 0. {
            return 1.;
        }
        (width.min(height) / self.side).clamp(super::camera::SCALES.0, 1.)
    }
}

/// Every related pair once, as indices into `agents`: two agents are related
/// when they share a session, or their sessions share a project (which links
/// a repository's agents across machines).
pub(crate) fn relations(agents: &[Agent]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for (i, a) in agents.iter().enumerate() {
        for (j, b) in agents.iter().enumerate().skip(i + 1) {
            let session = a.destination.host == b.destination.host
                && a.destination.name == b.destination.name;
            let project = a.project.is_some() && a.project == b.project;
            if session || project {
                pairs.push((i, j));
            }
        }
    }
    pairs
}

/// A point `t` of the way along the curve from `a` to `b` that bows away from
/// `core`, so links between machines arc round the outside instead of
/// crossing the core.
pub(crate) fn arc(a: Pt, b: Pt, core: Pt, t: f32) -> Pt {
    let mid = Pt {
        x: (a.x + b.x) / 2.,
        y: (a.y + b.y) / 2.,
    };
    let (dx, dy) = (mid.x - core.x, mid.y - core.y);
    let away = (dx * dx + dy * dy).sqrt().max(1.);
    let span = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
    let bow = span * 0.3;
    let control = Pt {
        x: mid.x + dx / away * bow,
        y: mid.y + dy / away * bow,
    };
    let u = 1. - t;
    Pt {
        x: u * u * a.x + 2. * u * t * control.x + t * t * b.x,
        y: u * u * a.y + 2. * u * t * control.y + t * t * b.y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::fleet::Destination;
    use crate::connection::{AgentAttention, AgentInfo, AgentState};

    fn agent(host: Option<&str>, session: &str, project: Option<&str>, state: AgentState) -> Agent {
        Agent {
            destination: Destination {
                host: host.map(str::to_owned),
                authority: None,
                key: None,
                name: session.into(),
            },
            terminal: phux_protocol::ResourceId::local(1),
            boot: None,
            info: AgentInfo {
                name: "a".into(),
                kind: None,
                state,
                attention: AgentAttention::None,
                changed_at_ms: 0,
            },
            title: String::new(),
            project: project.map(str::to_owned),
        }
    }

    fn distance(a: Pt, b: Pt) -> f32 {
        ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
    }

    #[test]
    fn systems_clear_the_core_and_each_other() {
        let mut agents = vec![agent(Some("nas"), "s", None, AgentState::Blocked)];
        agents.extend((0..8).map(|_| agent(None, "s", None, AgentState::Working)));
        let galaxy = Galaxy::new(
            vec![
                (None, true),
                (Some("nas".into()), true),
                (Some("mini".into()), false),
            ],
            &agents,
        );
        assert_eq!(
            galaxy.systems[0].host.as_deref(),
            Some("nas"),
            "needs you first"
        );
        assert!(galaxy.systems[0].centre.y < galaxy.core.y, "and on top");
        assert_eq!(
            galaxy.systems[2].host.as_deref(),
            Some("mini"),
            "offline last"
        );
        let reach = SHELL + RING_GAP + 50.;
        for (i, system) in galaxy.systems.iter().enumerate() {
            assert!((distance(system.centre, galaxy.core) - galaxy.orbit).abs() < 0.01);
            assert!(galaxy.orbit > reach + CORE_R);
            for other in &galaxy.systems[i + 1..] {
                assert!(distance(system.centre, other.centre) > 2. * reach - 1.);
            }
            for index in 0..system.agents.len() {
                for time in [0., 17., 45.] {
                    let p = system.point(index, time, true);
                    assert!(p.x > 0. && p.y > 0. && p.x < galaxy.side && p.y < galaxy.side);
                }
            }
        }
        let fit = galaxy.fit(900., 600.);
        assert!(fit >= super::super::camera::SCALES.0 && galaxy.side * fit <= 600.01);
    }

    #[test]
    fn only_roaming_agents_move_and_a_ring_never_stacks() {
        let agents: Vec<_> = (0..6)
            .map(|_| agent(None, "s", None, AgentState::Idle))
            .collect();
        let galaxy = Galaxy::new(vec![(None, true)], &agents);
        let system = &galaxy.systems[0];
        assert_eq!(system.point(0, 0., false), system.point(0, 30., false));
        assert_ne!(system.point(0, 0., true), system.point(0, 30., true));
        let spots: Vec<_> = (0..6).map(|i| system.point(i, 0., false)).collect();
        for (i, a) in spots.iter().enumerate() {
            for b in &spots[i + 1..] {
                assert!(distance(*a, *b) > 90.);
            }
        }
    }

    #[test]
    fn agents_relate_by_session_or_project_once_each() {
        let agents = [
            agent(None, "api", Some("phux"), AgentState::Working),
            agent(Some("studio"), "tests", Some("phux"), AgentState::Done),
            agent(Some("studio"), "db", None, AgentState::Idle),
            agent(Some("studio"), "db", None, AgentState::Idle),
            agent(Some("lab"), "db", None, AgentState::Idle),
        ];
        // The project crosses machines; a shared session name on another machine does not.
        assert_eq!(relations(&agents), vec![(0, 1), (2, 3)]);
    }

    #[test]
    fn an_arc_ends_on_its_agents_and_bows_away_from_the_core() {
        let core = Pt { x: 0., y: 0. };
        let (a, b) = (Pt { x: -100., y: 100. }, Pt { x: 100., y: 100. });
        assert_eq!(arc(a, b, core, 0.), a);
        assert_eq!(arc(a, b, core, 1.), b);
        assert!(arc(a, b, core, 0.5).y > 100., "bows outward");
    }
}
