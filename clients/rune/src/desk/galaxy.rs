//! The galaxy view's state and what it does: what it shows of each machine,
//! the camera, and the way from an agent to its terminal. The geometry is in
//! `crate::galaxy`.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use super::fleet::{Agent, Machine};
use crate::creature::{self, Work};
use crate::galaxy::{Camera, Plan};

/// How often the galaxy redraws while it moves.
const FRAME: Duration = Duration::from_millis(33);

#[derive(Debug, Default)]
pub struct Galaxy {
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

impl Desk {
    pub fn toggle_galaxy(&mut self) {
        if self.playground == Some(super::surfaces::Playground::Galaxy) {
            self.close_galaxy();
            return;
        }
        self.close_inbox();
        self.settings_open = false;
        self.playground = Some(super::surfaces::Playground::Galaxy);
        self.galaxy.placed = false;
        self.galaxy.picked = None;
        self.galaxy.opened_at = self.clock;
        self.galaxy.grades.clear();
    }

    pub fn close_galaxy(&mut self) {
        self.close_playground(super::surfaces::Playground::Galaxy);
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
        let moving = self.playground == Some(super::surfaces::Playground::Galaxy)
            && !self.galaxy.still.get();
        if moving {
            tx.wake_after(FRAME);
            let stars: Vec<Agent> = self
                .fleet_machines()
                .into_iter()
                .flat_map(|p| p.agents)
                .collect();
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

    pub fn galaxy_plan(planets: &[Machine]) -> Plan {
        Plan::new(&planets.iter().map(|p| p.agents.len()).collect::<Vec<_>>())
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
        let plan = Self::galaxy_plan(&self.fleet_machines());
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
        let planets = self.fleet_machines();
        let plan = Self::galaxy_plan(&planets);
        let waiting: Vec<_> = planets
            .iter()
            .zip(&plan.systems)
            .flat_map(|(planet, system)| {
                planet
                    .agents
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
        self.go_to_agent(id);
    }

    /// Return opens the agent the readout flew to.
    pub fn open_picked(&mut self) {
        let Some(id) = self.galaxy.picked.clone() else {
            return;
        };
        self.open_star(&id);
    }
}
