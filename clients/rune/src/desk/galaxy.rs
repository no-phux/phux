//! The galaxy view's state and what it does: what it shows of each machine,
//! the camera, and the way from an agent to its terminal. The geometry is in
//! `crate::galaxy`.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use phux_client_runtime::Client;
use phux_client_runtime::control::Topology;
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use crate::activity::{Activities, Mark};
use crate::creature::{self, Stats, Work};
use crate::galaxy::{Camera, Plan};
use crate::machines::LOCAL;

/// How often the galaxy redraws while it moves.
const FRAME: Duration = Duration::from_millis(33);

#[derive(Debug, Default)]
pub struct Galaxy {
    pub open: bool,
    pub camera: Camera,
    /// Whether the camera has been fitted to the view since opening.
    placed: bool,
    /// The view's size in points, from its last layout.
    pub viewport: [f32; 2],
    /// The camera's offset when the current drag began.
    grab: Option<[f32; 2]>,
    /// The waiting agent the readout flew to last; Return opens it.
    pub picked: Option<ResourceId>,
    /// App clock seconds when it opened: the orbits' zero.
    opened_at: f64,
    /// Reduced motion, as the last view build saw it: the clock stops.
    pub still: Cell<bool>,
    /// Each agent's last work seen, and the app clock at its last level-up.
    grades: HashMap<ResourceId, (Option<Work>, f64)>,
}

/// An agent on one machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Star {
    /// Unique across machines: this window's terminal id, or another
    /// machine's tagged with its name (`ResourceId::Satellite`).
    pub id: ResourceId,
    /// The terminal on its own server.
    pub terminal: ResourceId,
    /// Its machine when that is not this window's; `None` for this window's
    /// own and for this computer seen from a remote alike, told apart by `here`.
    pub machine: Option<String>,
    /// Whether it is on the machine this window is connected to.
    pub here: bool,
    pub kind: Option<String>,
    pub session: String,
    pub name: String,
    pub mark: Mark,
    /// The session's project (the desktop's rule), else the directory's.
    pub project: Option<String>,
    /// Its work from its machine's agent stats ([`creature`]), when published.
    pub work: Option<Work>,
}

/// A machine and its agents: the connected one's from this window, another's
/// from its watch (`desk::fleet`), else what its session listing says.
#[derive(Debug, Clone, PartialEq)]
pub struct Planet {
    pub name: String,
    pub online: bool,
    pub stars: Vec<Star>,
    /// Whether `stars` is its agents: the connected machine's, or a watched one's.
    pub known: bool,
    pub note: Option<String>,
}

impl Desk {
    pub fn toggle_galaxy(&mut self) {
        if self.galaxy.open {
            self.close_galaxy();
            return;
        }
        self.close_inbox();
        self.close_pitch();
        self.close_kitchen();
        self.settings_open = false;
        self.galaxy.open = true;
        self.galaxy.placed = false;
        self.galaxy.picked = None;
        self.galaxy.opened_at = self.clock;
        self.galaxy.grades.clear();
    }

    pub fn close_galaxy(&mut self) {
        self.galaxy.open = false;
        self.galaxy.grab = None;
    }

    /// Seconds into the orbits, or zero while motion is reduced.
    pub fn galaxy_time(&self) -> f32 {
        if self.galaxy.still.get() {
            0.0
        } else {
            (self.clock - self.galaxy.opened_at) as f32
        }
    }

    /// Keep drawing while the galaxy is open and moving, and note which
    /// agents just grew a tier.
    pub(super) fn tick_galaxy(&mut self, tx: &mut Tx) -> bool {
        let moving = self.galaxy.open && !self.galaxy.still.get();
        if moving {
            tx.wake_after(FRAME);
            let stars: Vec<Star> = self.planets().into_iter().flat_map(|p| p.stars).collect();
            let grades = &mut self.galaxy.grades;
            grades.retain(|id, _| stars.iter().any(|s| &s.id == id));
            for star in stars {
                let (work, up) = grades
                    .entry(star.id)
                    .or_insert((star.work, f64::NEG_INFINITY));
                if creature::levels_up(work.as_ref(), star.work.as_ref()) {
                    *up = self.clock;
                }
                *work = star.work;
            }
        }
        moving
    }

    /// How much of agent `id`'s level-up burst is left, 1 to 0 over 2.2 s.
    pub fn level_up(&self, id: &ResourceId) -> f32 {
        self.galaxy.grades.get(id).map_or(0.0, |(_, up)| {
            (1.0 - (self.clock - up) as f32 * 0.45).clamp(0.0, 1.0)
        })
    }

    /// Every machine, those with something waiting first; the connected one
    /// with its agents.
    pub fn planets(&self) -> Vec<Planet> {
        let current = self.machines.current.clone();
        let here = Planet {
            name: current.clone().unwrap_or_else(|| LOCAL.to_owned()),
            online: self.client.is_some(),
            stars: self.stars(),
            known: true,
            note: None,
        };
        let others = std::iter::once(None)
            .chain(self.machines.hosts.iter().map(|h| Some(h.name.clone())))
            .filter(|host| *host != current)
            .map(|host| {
                let watch = self.fleet.watches.get(&host);
                let stars = watch.and_then(|w| w.stars(&host, self.clock));
                let glance = self.machines.glances.get(&host);
                let note = match glance {
                    Some(Ok(sessions)) => {
                        let running: u64 = sessions.iter().map(|s| s.running).sum();
                        Some(format!("{} sessions · {running} running", sessions.len()))
                    }
                    _ => None,
                };
                let online = watch.map_or(!matches!(glance, Some(Err(_))), |w| {
                    w.online() || matches!(glance, Some(Ok(_)))
                });
                Planet {
                    name: host.unwrap_or_else(|| LOCAL.to_owned()),
                    online,
                    known: stars.is_some(),
                    note: if stars.is_some() { None } else { note },
                    stars: stars.unwrap_or_default(),
                }
            });
        let mut planets: Vec<Planet> = std::iter::once(here).chain(others).collect();
        planets.sort_by_key(|p| !p.stars.iter().any(|s| s.mark.needs_you()));
        planets
    }

    /// The connected machine's agents.
    pub(super) fn stars(&self) -> Vec<Star> {
        let Some(topology) = &self.topology else {
            return Vec::new();
        };
        let mut stars = stars_on(
            topology,
            &self.activities,
            self.client.as_deref(),
            (&self.fleet.here, self.clock),
        );
        for star in &mut stars {
            star.here = true;
        }
        stars
    }

    pub fn galaxy_plan(planets: &[Planet]) -> Plan {
        Plan::new(&planets.iter().map(|p| p.stars.len()).collect::<Vec<_>>())
    }

    /// The view laid out at `w` by `h`: the first layout fits the galaxy.
    pub fn galaxy_laid_out(&mut self, w: f32, h: f32) {
        self.galaxy.viewport = [w, h];
        if self.galaxy.placed || w <= 0.0 || h <= 0.0 {
            return;
        }
        self.galaxy.placed = true;
        self.fit_galaxy();
    }

    pub fn fit_galaxy(&mut self) {
        let plan = Self::galaxy_plan(&self.planets());
        let [w, h] = self.galaxy.viewport;
        self.galaxy.camera = Camera::look(plan.core, w, h, plan.fit(w, h));
    }

    /// Centre `world` at full size.
    pub fn fly_to(&mut self, world: [f32; 2]) {
        let [w, h] = self.galaxy.viewport;
        self.galaxy.camera = Camera::look(world, w, h, 1.0);
    }

    pub fn galaxy_drag(&mut self, drag: &rune::Drag) {
        match drag.phase {
            rune::Phase::Down => self.galaxy.grab = Some(self.galaxy.camera.offset),
            rune::Phase::Move => {
                if let Some([x, y]) = self.galaxy.grab {
                    self.galaxy.camera.offset = [x + drag.dx, y + drag.dy];
                }
            }
            rune::Phase::Up => self.galaxy.grab = None,
        }
    }

    /// The waiting agent after the last one flown to, round again at the end.
    pub fn next_waiting(&mut self) {
        let planets = self.planets();
        let plan = Self::galaxy_plan(&planets);
        let waiting: Vec<_> = planets
            .iter()
            .zip(&plan.systems)
            .flat_map(|(planet, system)| {
                planet
                    .stars
                    .iter()
                    .enumerate()
                    .filter(|(_, star)| star.mark.needs_you())
                    .map(move |(i, star)| (star.id.clone(), system.point(i, 0.0, false)))
            })
            .collect();
        if waiting.is_empty() {
            return;
        }
        let next = waiting
            .iter()
            .position(|(id, _)| Some(id) == self.galaxy.picked.as_ref())
            .map_or(0, |at| (at + 1) % waiting.len());
        let (id, at) = waiting[next].clone();
        self.galaxy.picked = Some(id);
        self.fly_to(at);
    }

    /// Close the galaxy on agent `id`: this machine's shown the way the inbox
    /// shows one, another machine's by moving the window there first.
    pub fn open_star(&mut self, id: &ResourceId) {
        self.close_galaxy();
        self.go_to_star(id);
    }

    /// Show agent `id` wherever it lives.
    pub(super) fn go_to_star(&mut self, id: &ResourceId) {
        let Some(star) = self
            .planets()
            .into_iter()
            .flat_map(|p| p.stars)
            .find(|s| &s.id == id)
        else {
            return;
        };
        if star.here {
            self.show(star.terminal, &star.session);
        } else {
            self.open_on(star.machine, star.session);
            self.want(star.terminal);
        }
    }

    /// Return opens the agent the readout flew to.
    pub fn open_picked(&mut self) {
        let Some(id) = self.galaxy.picked.clone() else {
            return;
        };
        self.open_star(&id);
    }
}

/// A server's agents: each pane with a declared agent, or one that asks,
/// with their work from its `stats` as of `now`.
pub(super) fn stars_on(
    topology: &Topology,
    activities: &Activities,
    client: Option<&Client>,
    (stats, now): (&Stats, f64),
) -> Vec<Star> {
    topology
        .panes
        .iter()
        .filter(|pane| {
            activities.agent_name(&pane.terminal_id).is_some()
                || crate::inbox::question(pane, activities).is_some()
        })
        .map(|pane| Star {
            id: pane.terminal_id.clone(),
            terminal: pane.terminal_id.clone(),
            machine: None,
            here: false,
            kind: activities.agent_kind(&pane.terminal_id).map(str::to_owned),
            session: pane.session_name.clone(),
            name: star_name(
                activities.agent_name(&pane.terminal_id),
                activities.agent_kind(&pane.terminal_id),
                &crate::palette::pane_label(pane),
            ),
            mark: activities.mark(&pane.terminal_id),
            work: stats.work(
                &pane.terminal_id,
                activities.mark(&pane.terminal_id) == Mark::Working,
                now,
            ),
            project: session_project(topology, client, pane.session_id).or_else(|| {
                pane.cwd
                    .as_deref()
                    .and_then(crate::galaxy::project)
                    .map(str::to_owned)
            }),
        })
        .collect()
}

/// What a star is called: its agent's own name, unless that only says which
/// harness it is (`claude` from `claude`), when the pane's title says what it
/// is doing; the title loses the status glyph a harness puts in front.
fn star_name(agent: Option<&str>, kind: Option<&str>, label: &str) -> String {
    let title = label
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim();
    match agent {
        Some(name) if !kind.is_some_and(|kind| name.eq_ignore_ascii_case(kind)) => name.to_owned(),
        _ if !title.is_empty() => title.to_owned(),
        Some(name) => name.to_owned(),
        None => label.to_owned(),
    }
}

/// The project its session record names, as the desktop groups sessions.
fn session_project(topology: &Topology, client: Option<&Client>, session: u32) -> Option<String> {
    let key = topology.sessions.iter().find(|s| s.id == session)?.key?;
    let organization = client?.organization();
    let record = organization.session(&key)?;
    let attr = |name: &str| {
        record
            .attrs
            .iter()
            .find(|(k, v)| k == name && !v.is_empty())
            .map(|(_, v)| v.clone())
    };
    attr("project.name").or_else(|| attr("project.id"))
}

#[cfg(test)]
mod tests {
    use super::star_name;

    #[test]
    fn a_star_is_named_for_its_work_when_its_agent_only_names_the_harness() {
        let title = "\u{2733} Clone the website";
        assert_eq!(
            star_name(Some("claude"), Some("claude"), title),
            "Clone the website"
        );
        assert_eq!(
            star_name(Some("Claude"), Some("claude"), title),
            "Clone the website"
        );
        assert_eq!(
            star_name(Some("build-box"), Some("claude"), title),
            "build-box"
        );
        assert_eq!(star_name(Some("claude"), Some("claude"), "  "), "claude");
        assert_eq!(star_name(None, None, "deploy"), "deploy");
    }
}
