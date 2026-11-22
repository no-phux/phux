//! The hive view's state and what it does: the fleet as bees, kept in step
//! each frame, and the way from a bee to its terminal. The scene is
//! `crate::hive`; the painter is `ui::hive`. The shared ways in and out are
//! `desk::scene_host`.

use std::cell::RefCell;

use phux_protocol::ids::ResourceId;

use super::Desk;
use super::fleet::Machine;
use super::scene_host::{FleetScene, SceneHost, SceneState};
use super::surfaces::Playground;
use crate::creature;
use crate::hive::{self, Forager};
use crate::playground::{self, Mood};

pub type Hive = SceneHost<hive::Hive>;

/// The meadow, hive box and comb by day and by night, painted once per
/// layout generation.
pub type Backdrop = RefCell<Option<(u64, rune::Shape, rune::Shape)>>;

/// Machines as frames in a fixed order (a frame never moves when an agent
/// starts waiting); every agent the galaxy knows of as a bee.
impl FleetScene for hive::Hive {
    type Who = Forager;
    type Backdrop = Backdrop;
    const PLAYGROUND: Playground = Playground::Hive;
    const MACHINE_ORDER: bool = true;
    const LAYOUT_FIRST: bool = true;

    fn host(desk: &mut Desk) -> &mut Hive {
        &mut desk.hive
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
        let waiting = self.bees.iter().filter(|b| b.who.mood == Mood::Waiting);
        waiting.map(|b| b.who.id.clone()).collect()
    }

    fn selected_id(&self) -> Option<ResourceId> {
        self.selected_bee().map(|b| b.who.id.clone())
    }

    fn creatures(planets: &[Machine]) -> Vec<Forager> {
        planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| p.agents.iter().map(move |s| (at, s)))
            .map(|(at, s)| Forager {
                id: s.id.clone(),
                name: s.name.clone(),
                host: at,
                mood: Mood::of(s.mark),
                species: playground::species(s.kind.as_deref(), &s.name),
                tier: creature::tier(s.work.as_ref()),
                work: s.work,
            })
            .collect()
    }

    fn layout(&mut self, w: f32, h: f32, hosts: &[(String, bool)]) {
        hive::Hive::layout(self, w, h, hosts);
    }

    fn sync(&mut self, who: Vec<Forager>) {
        hive::Hive::sync(self, who);
    }

    fn advance(&mut self, dt: f32) {
        hive::Hive::advance(self, dt);
    }
}

impl Desk {
    pub fn toggle_hive(&mut self) {
        self.toggle_scene::<hive::Hive>();
    }

    pub fn close_hive(&mut self) {
        self.close_scene::<hive::Hive>();
    }

    pub fn hive_laid_out(&mut self, w: f32, h: f32) {
        self.scene_laid_out::<hive::Hive>(w, h);
    }

    pub fn hive_press(&mut self, id: ResourceId) {
        self.scene_press::<hive::Hive>(id);
    }

    pub fn hive_deselect(&mut self) {
        self.scene_deselect::<hive::Hive>();
    }

    pub fn hive_hover(&mut self, id: ResourceId, on: bool) {
        self.scene_hover::<hive::Hive>(id, on);
    }

    pub fn hive_next_waiting(&mut self) {
        self.scene_next_waiting::<hive::Hive>();
    }

    pub fn hive_escape(&mut self) {
        self.scene_escape::<hive::Hive>();
    }

    /// Close the hive on the selected bee's terminal.
    pub fn open_bee(&mut self) {
        self.open_scene_pick::<hive::Hive>();
    }
}
