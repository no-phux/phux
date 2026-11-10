//! The hive view's state and what it does: the fleet as bees, kept in step
//! each frame, and the way from a bee to its terminal. The scene is
//! `crate::hive`; the painter is `ui::hive`.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use super::galaxy::Planet;
use crate::creature;
use crate::hive::{self, Forager};
use crate::machines::LOCAL;
use crate::reef::{self, Mood};

/// How often the hive redraws while open: every frame.
const FRAME: Duration = Duration::from_millis(16);

#[derive(Debug, Default)]
pub struct Hive {
    pub open: bool,
    pub scene: hive::Hive,
    /// The view's size in points, from its last layout.
    pub viewport: [f32; 2],
    /// App clock seconds at the last frame.
    last: Option<f64>,
    /// Whether the theme is dark, as the last view build saw it: night.
    pub night: Cell<bool>,
    /// Reduced motion, as the last view build saw it.
    pub still: Cell<bool>,
    /// The meadow, hive box and comb by day and by night, painted once per
    /// layout generation.
    pub backdrop: RefCell<Option<(u64, rune::Shape, rune::Shape)>>,
}

impl Desk {
    pub fn toggle_hive(&mut self) {
        if self.hive.open {
            self.close_hive();
            return;
        }
        self.close_inbox();
        self.close_galaxy();
        self.close_reef();
        self.close_pitch();
        self.close_kitchen();
        self.close_pitlane();
        self.settings_open = false;
        self.hive.open = true;
        self.hive.last = None;
        self.hive.scene.selected = None;
    }

    pub fn close_hive(&mut self) {
        self.hive.open = false;
    }

    /// Keep the bees in step with the fleet and the scene moving while open.
    pub(super) fn tick_hive(&mut self, tx: &mut Tx) -> bool {
        if !self.hive.open {
            return false;
        }
        self.refresh_hive();
        let dt = self
            .hive
            .last
            .map_or(0.0, |last| (self.clock - last) as f32);
        self.hive.last = Some(self.clock);
        self.hive.scene.advance(dt);
        tx.wake_after(FRAME);
        true
    }

    /// The view laid out at `w` by `h`.
    pub fn hive_laid_out(&mut self, w: f32, h: f32) {
        self.hive.viewport = [w, h];
        self.refresh_hive();
    }

    /// Machines as frames in the galaxy's order, without its needs-you-first
    /// sort (a frame never moves when an agent starts waiting); every agent
    /// it knows of as a bee.
    fn refresh_hive(&mut self) {
        let mut planets = self.planets();
        let here = (self.machines.current.clone()).unwrap_or_else(|| LOCAL.to_owned());
        let order: Vec<String> = [here, LOCAL.to_owned()]
            .into_iter()
            .chain(self.machines.hosts.iter().map(|h| h.name.clone()))
            .collect();
        sort_machines(&mut planets, &order);
        let hosts: Vec<(String, bool)> =
            planets.iter().map(|p| (p.name.clone(), p.known)).collect();
        let foragers = planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| p.stars.iter().map(move |s| (at, s)))
            .map(|(at, s)| Forager {
                id: s.id.clone(),
                name: s.name.clone(),
                host: at,
                mood: Mood::of(s.mark),
                species: reef::species(s.kind.as_deref(), &s.name),
                tier: creature::tier(s.work.as_ref()),
                work: s.work,
            })
            .collect();
        let scene = &mut self.hive.scene;
        scene.nt_target = if self.hive.night.get() { 1.0 } else { 0.0 };
        scene.reduce = self.hive.still.get();
        let [w, h] = self.hive.viewport;
        scene.layout(w, h, &hosts);
        scene.sync(foragers);
    }

    /// A press on a bee selects it; a press on the selected one opens its
    /// terminal, as the galaxy opens a star.
    pub fn hive_press(&mut self, id: ResourceId) {
        if self.hive.scene.selected.as_ref() == Some(&id) {
            self.open_bee();
        } else {
            self.hive.scene.selected = Some(id);
        }
    }

    pub fn hive_deselect(&mut self) {
        self.hive.scene.selected = None;
    }

    pub fn hive_hover(&mut self, id: ResourceId, on: bool) {
        let hovered = &mut self.hive.scene.hovered;
        if on {
            *hovered = Some(id);
        } else if hovered.as_ref() == Some(&id) {
            *hovered = None;
        }
    }

    /// The waiting bee after the selected one, round again at the end.
    pub fn hive_next_waiting(&mut self) {
        let scene = &self.hive.scene;
        let waiting: Vec<ResourceId> = scene
            .bees
            .iter()
            .filter(|b| b.who.mood == Mood::Waiting)
            .map(|b| b.who.id.clone())
            .collect();
        if waiting.is_empty() {
            return;
        }
        let next = waiting
            .iter()
            .position(|id| Some(id) == scene.selected.as_ref())
            .map_or(0, |at| (at + 1) % waiting.len());
        self.hive.scene.selected = Some(waiting[next].clone());
    }

    /// Escape lets go of a bee first, then closes.
    pub fn hive_escape(&mut self) {
        if self.hive.scene.selected.take().is_none() {
            self.close_hive();
        }
    }

    /// Close the hive on the selected bee's terminal.
    pub fn open_bee(&mut self) {
        let Some(b) = self.hive.scene.selected_bee() else {
            return;
        };
        let id = b.who.id.clone();
        self.close_hive();
        self.go_to_star(&id);
    }
}

/// `planets` back in machine order: `order`'s, the rest after in the order
/// they came. A stable sort, so the galaxy's tie order holds.
fn sort_machines(planets: &mut [Planet], order: &[String]) {
    planets.sort_by_key(|p| {
        order
            .iter()
            .position(|n| *n == p.name)
            .unwrap_or(usize::MAX)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planet(name: &str) -> Planet {
        Planet {
            name: name.to_owned(),
            online: true,
            stars: Vec::new(),
            known: true,
            note: None,
        }
    }

    #[test]
    fn machines_keep_their_order_when_one_starts_waiting() {
        let order: Vec<String> = ["mini", "local", "macbook", "powerhouse-1"]
            .map(str::to_owned)
            .to_vec();
        // The galaxy put the waiting machine first.
        let mut planets = vec![planet("powerhouse-1"), planet("mini"), planet("macbook")];
        sort_machines(&mut planets, &order);
        let names: Vec<&str> = planets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["mini", "macbook", "powerhouse-1"]);
    }
}
