//! The pit lane view's state and what it does: the fleet as race cars, kept
//! in step each frame, and the way from a car to its terminal. The scene is
//! `crate::pitlane`; the painter is `ui::pitlane`. The shared ways in and
//! out are `desk::scene_host`.

use std::cell::RefCell;

use phux_protocol::ids::ResourceId;

use super::Desk;
use super::fleet::Machine;
use super::scene_host::{FleetScene, SceneHost, SceneState};
use super::surfaces::Playground;
use crate::pitlane::{self, Driver};
use crate::playground::{self, Mood};

pub type PitLane = SceneHost<pitlane::PitLane>;

/// The grounds by day and by night, drawn once per layout: the layout's
/// revision and the two shapes.
pub type Backdrop = RefCell<(u64, rune::Shape, rune::Shape)>;

/// Machines as garages in the galaxy's order, every agent it knows of as a car.
impl FleetScene for pitlane::PitLane {
    type Who = Driver;
    type Backdrop = Backdrop;
    const PLAYGROUND: Playground = Playground::PitLane;
    const MACHINE_ORDER: bool = false;
    const LAYOUT_FIRST: bool = false;

    fn host(desk: &mut Desk) -> &mut PitLane {
        &mut desk.pitlane
    }

    fn state(&mut self) -> SceneState<'_> {
        SceneState {
            selected: &mut self.selected,
            hovered: &mut self.hovered,
            nt_target: &mut self.nt_target,
            reduce: &mut self.reduce,
        }
    }

    fn waiting_ids(&self) -> Vec<ResourceId> {
        let waiting = self.cars.iter().filter(|c| c.who.mood == Mood::Waiting);
        waiting.map(|c| c.who.id.clone()).collect()
    }

    fn selected_id(&self) -> Option<ResourceId> {
        self.selected_car().map(|c| c.who.id.clone())
    }

    fn creatures(planets: &[Machine]) -> Vec<Driver> {
        planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| {
                p.agents.iter().map(move |s| Driver {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    host: at,
                    mood: Mood::of(s.mark),
                    species: playground::species(s.kind.as_deref(), &s.name),
                    work: s.work,
                })
            })
            .collect()
    }

    fn layout(&mut self, w: f32, h: f32, hosts: &[(String, bool)]) {
        pitlane::PitLane::layout(self, w, h, hosts);
    }

    fn sync(&mut self, who: Vec<Driver>) {
        pitlane::PitLane::sync(self, who);
    }

    fn advance(&mut self, dt: f32) {
        pitlane::PitLane::advance(self, dt);
    }
}

impl Desk {
    pub fn toggle_pitlane(&mut self) {
        self.toggle_scene::<pitlane::PitLane>();
    }

    pub fn close_pitlane(&mut self) {
        self.close_scene::<pitlane::PitLane>();
    }

    pub fn pitlane_laid_out(&mut self, w: f32, h: f32) {
        self.scene_laid_out::<pitlane::PitLane>(w, h);
    }

    pub fn pitlane_press(&mut self, id: ResourceId) {
        self.scene_press::<pitlane::PitLane>(id);
    }

    pub fn pitlane_deselect(&mut self) {
        self.scene_deselect::<pitlane::PitLane>();
    }

    pub fn pitlane_hover(&mut self, id: ResourceId, on: bool) {
        self.scene_hover::<pitlane::PitLane>(id, on);
    }

    pub fn pitlane_next_waiting(&mut self) {
        self.scene_next_waiting::<pitlane::PitLane>();
    }

    pub fn pitlane_escape(&mut self) {
        self.scene_escape::<pitlane::PitLane>();
    }

    /// Close the pit lane on the selected car's terminal.
    pub fn open_car(&mut self) {
        self.open_scene_pick::<pitlane::PitLane>();
    }
}
