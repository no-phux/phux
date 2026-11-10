//! The pit lane view's state and what it does: the fleet as race cars, kept
//! in step each frame, and the way from a car to its terminal. The scene is
//! `crate::pitlane`; the painter is `ui::pitlane`.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use crate::pitlane::{self, Driver};
use crate::reef::{self, Mood};

/// How often the pit lane redraws while open: every frame.
const FRAME: Duration = Duration::from_millis(16);

#[derive(Debug, Default)]
pub struct PitLane {
    pub open: bool,
    pub scene: pitlane::PitLane,
    /// The view's size in points, from its last layout.
    pub viewport: [f32; 2],
    /// App clock seconds at the last frame.
    last: Option<f64>,
    /// Whether the theme is dark, as the last view build saw it: night.
    pub night: Cell<bool>,
    /// Reduced motion, as the last view build saw it.
    pub still: Cell<bool>,
    /// The grounds by day and by night, drawn once per layout: the
    /// layout's revision and the two shapes.
    pub ground: RefCell<(u64, rune::Shape, rune::Shape)>,
}

impl Desk {
    pub fn toggle_pitlane(&mut self) {
        if self.pitlane.open {
            self.close_pitlane();
            return;
        }
        self.close_inbox();
        self.close_galaxy();
        self.close_reef();
        self.close_pitch();
        self.close_kitchen();
        self.close_hive();
        self.settings_open = false;
        self.pitlane.open = true;
        self.pitlane.last = None;
        self.pitlane.scene.selected = None;
    }

    pub fn close_pitlane(&mut self) {
        self.pitlane.open = false;
    }

    /// Keep the cars in step with the fleet and the race moving while open.
    pub(super) fn tick_pitlane(&mut self, tx: &mut Tx) -> bool {
        if !self.pitlane.open {
            return false;
        }
        self.refresh_pitlane();
        let dt = self
            .pitlane
            .last
            .map_or(0.0, |last| (self.clock - last) as f32);
        self.pitlane.last = Some(self.clock);
        self.pitlane.scene.advance(dt);
        tx.wake_after(FRAME);
        true
    }

    /// The view laid out at `w` by `h`.
    pub fn pitlane_laid_out(&mut self, w: f32, h: f32) {
        self.pitlane.viewport = [w, h];
        self.refresh_pitlane();
    }

    /// Machines as garages in the galaxy's order, every agent it knows of as a car.
    fn refresh_pitlane(&mut self) {
        let planets = self.planets();
        let hosts: Vec<(String, bool)> =
            planets.iter().map(|p| (p.name.clone(), p.known)).collect();
        let drivers = planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| {
                p.stars.iter().map(move |s| Driver {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    host: at,
                    mood: Mood::of(s.mark),
                    species: reef::species(s.kind.as_deref(), &s.name),
                    work: s.work,
                })
            })
            .collect();
        let scene = &mut self.pitlane.scene;
        scene.nt_target = if self.pitlane.night.get() { 1.0 } else { 0.0 };
        scene.reduce = self.pitlane.still.get();
        scene.sync(drivers);
        let [w, h] = self.pitlane.viewport;
        scene.layout(w, h, &hosts);
    }

    /// A press on a car selects it; a press on the selected one opens its
    /// terminal, as the galaxy opens a star.
    pub fn pitlane_press(&mut self, id: ResourceId) {
        if self.pitlane.scene.selected.as_ref() == Some(&id) {
            self.open_car();
        } else {
            self.pitlane.scene.selected = Some(id);
        }
    }

    pub fn pitlane_deselect(&mut self) {
        self.pitlane.scene.selected = None;
    }

    pub fn pitlane_hover(&mut self, id: ResourceId, on: bool) {
        let hovered = &mut self.pitlane.scene.hovered;
        if on {
            *hovered = Some(id);
        } else if hovered.as_ref() == Some(&id) {
            *hovered = None;
        }
    }

    /// The waiting car after the selected one, round again at the end.
    pub fn pitlane_next_waiting(&mut self) {
        let scene = &self.pitlane.scene;
        let waiting: Vec<ResourceId> = scene
            .cars
            .iter()
            .filter(|c| c.who.mood == Mood::Waiting)
            .map(|c| c.who.id.clone())
            .collect();
        if waiting.is_empty() {
            return;
        }
        let next = waiting
            .iter()
            .position(|id| Some(id) == scene.selected.as_ref())
            .map_or(0, |at| (at + 1) % waiting.len());
        self.pitlane.scene.selected = Some(waiting[next].clone());
    }

    /// Escape lets go of a car first, then closes.
    pub fn pitlane_escape(&mut self) {
        if self.pitlane.scene.selected.take().is_none() {
            self.close_pitlane();
        }
    }

    /// Close the pit lane on the selected car's terminal.
    pub fn open_car(&mut self) {
        let Some(c) = self.pitlane.scene.selected_car() else {
            return;
        };
        let id = c.who.id.clone();
        self.close_pitlane();
        self.go_to_star(&id);
    }
}
