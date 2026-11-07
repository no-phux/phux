//! The galaxy view's state and what it does: what it shows of each machine,
//! the camera, and the way from an agent to its terminal. The geometry is in
//! `crate::galaxy`.

use std::cell::Cell;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use crate::activity::Mark;
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
}

/// An agent on the machine this window is connected to.
#[derive(Debug, Clone, PartialEq)]
pub struct Star {
    pub terminal: ResourceId,
    pub session: String,
    pub name: String,
    pub mark: Mark,
    pub cwd: Option<String>,
}

/// A machine. Only the connected one lists its agents; another shows what
/// its session listing says, when the sidebar has looked.
#[derive(Debug, Clone, PartialEq)]
pub struct Planet {
    pub name: String,
    pub online: bool,
    pub stars: Vec<Star>,
    pub note: Option<String>,
}

impl Desk {
    pub fn toggle_galaxy(&mut self) {
        if self.galaxy.open {
            self.close_galaxy();
            return;
        }
        self.close_inbox();
        self.settings_open = false;
        self.galaxy.open = true;
        self.galaxy.placed = false;
        self.galaxy.picked = None;
        self.galaxy.opened_at = self.clock;
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

    /// Keep drawing while the galaxy is open and moving.
    pub(super) fn tick_galaxy(&self, tx: &mut Tx) -> bool {
        let moving = self.galaxy.open && !self.galaxy.still.get();
        if moving {
            tx.wake_after(FRAME);
        }
        moving
    }

    /// Every machine, those with something waiting first; the connected one
    /// with its agents.
    pub fn planets(&self) -> Vec<Planet> {
        let current = self.machines.current.clone();
        let here = Planet {
            name: current.clone().unwrap_or_else(|| LOCAL.to_owned()),
            online: self.client.is_some(),
            stars: self.stars(),
            note: None,
        };
        let others = std::iter::once(None)
            .chain(self.machines.hosts.iter().map(|h| Some(h.name.clone())))
            .filter(|host| *host != current)
            .map(|host| {
                let glance = self.machines.glances.get(&host);
                let note = match glance {
                    Some(Ok(sessions)) => {
                        let running: u64 = sessions.iter().map(|s| s.running).sum();
                        Some(format!("{} sessions · {running} running", sessions.len()))
                    }
                    _ => None,
                };
                Planet {
                    name: host.unwrap_or_else(|| LOCAL.to_owned()),
                    online: !matches!(glance, Some(Err(_))),
                    stars: Vec::new(),
                    note,
                }
            });
        let mut planets: Vec<Planet> = std::iter::once(here).chain(others).collect();
        planets.sort_by_key(|p| !p.stars.iter().any(|s| s.mark.needs_you()));
        planets
    }

    /// The connected machine's agents: a pane with a declared agent, or one
    /// that asks.
    fn stars(&self) -> Vec<Star> {
        let Some(topology) = &self.topology else {
            return Vec::new();
        };
        topology
            .panes
            .iter()
            .filter(|pane| {
                self.activities.agent_name(&pane.terminal_id).is_some()
                    || crate::inbox::question(pane, &self.activities).is_some()
            })
            .map(|pane| Star {
                terminal: pane.terminal_id.clone(),
                session: pane.session_name.clone(),
                name: self
                    .activities
                    .agent_name(&pane.terminal_id)
                    .map_or_else(|| crate::palette::pane_label(pane), str::to_owned),
                mark: self.activities.mark(&pane.terminal_id),
                cwd: pane.cwd.clone(),
            })
            .collect()
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
                    .map(move |(i, star)| (star.terminal.clone(), system.point(i, 0.0, false)))
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

    /// Close the galaxy on `terminal`, shown the way the inbox shows one.
    pub fn open_star(&mut self, terminal: ResourceId, session: &str) {
        self.close_galaxy();
        self.show(terminal, session);
    }

    /// Return opens the agent the readout flew to.
    pub fn open_picked(&mut self) {
        let Some(id) = self.galaxy.picked.clone() else {
            return;
        };
        if let Some(star) = self.stars().into_iter().find(|s| s.terminal == id) {
            self.open_star(star.terminal, &star.session);
        }
    }
}
