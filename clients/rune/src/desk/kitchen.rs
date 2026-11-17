//! The kitchen view's state and what it does: the fleet as chefs, kept in
//! step each frame, and the way from a chef to its terminal. The scene is
//! `crate::kitchen`; the painter is `ui::kitchen`. The shared ways in and
//! out are `desk::scene_host`.

use std::cell::RefCell;

use phux_protocol::ids::ResourceId;

use super::Desk;
use super::fleet::Machine;
use super::scene_host::{FleetScene, SceneHost, SceneState};
use super::surfaces::Playground;
use crate::kitchen::{self, Cook};
use crate::playground::{self, Mood};

pub type Kitchen = SceneHost<kitchen::Kitchen>;

#[derive(Debug, Default)]
pub struct Backdrop {
    /// The room's static paint and the layout generation it was made for.
    pub room: RefCell<(u32, rune::Shape)>,
    /// The night's darkness, redrawn each frame while it's dark.
    pub mask: rune::Pixels,
}

/// Machines as lines in a fixed order, so a line never swaps places when an
/// agent there starts needing you; every agent the galaxy knows of as a chef.
impl FleetScene for kitchen::Kitchen {
    type Who = Cook;
    type Backdrop = Backdrop;
    const PLAYGROUND: Playground = Playground::Kitchen;
    const MACHINE_ORDER: bool = true;
    const LAYOUT_FIRST: bool = true;

    fn host(desk: &mut Desk) -> &mut Kitchen {
        &mut desk.kitchen
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
        let waiting = self.chefs.iter().filter(|c| c.who.mood == Mood::Waiting);
        waiting.map(|c| c.who.id.clone()).collect()
    }

    fn selected_id(&self) -> Option<ResourceId> {
        self.selected_chef().map(|c| c.who.id.clone())
    }

    fn creatures(planets: &[Machine]) -> Vec<Cook> {
        planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| {
                p.agents.iter().map(move |s| Cook {
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
        kitchen::Kitchen::layout(self, w, h, hosts);
    }

    fn sync(&mut self, who: Vec<Cook>) {
        kitchen::Kitchen::sync(self, who);
    }

    fn advance(&mut self, dt: f32) {
        kitchen::Kitchen::advance(self, dt);
    }
}

impl Desk {
    pub fn toggle_kitchen(&mut self) {
        self.toggle_scene::<kitchen::Kitchen>();
    }

    pub fn close_kitchen(&mut self) {
        self.close_scene::<kitchen::Kitchen>();
    }

    pub fn kitchen_laid_out(&mut self, w: f32, h: f32) {
        self.scene_laid_out::<kitchen::Kitchen>(w, h);
    }

    pub fn kitchen_press(&mut self, id: ResourceId) {
        self.scene_press::<kitchen::Kitchen>(id);
    }

    pub fn kitchen_deselect(&mut self) {
        self.scene_deselect::<kitchen::Kitchen>();
    }

    pub fn kitchen_hover(&mut self, id: ResourceId, on: bool) {
        self.scene_hover::<kitchen::Kitchen>(id, on);
    }

    pub fn kitchen_next_waiting(&mut self) {
        self.scene_next_waiting::<kitchen::Kitchen>();
    }

    pub fn kitchen_escape(&mut self) {
        self.scene_escape::<kitchen::Kitchen>();
    }

    /// Close the kitchen on the selected chef's terminal.
    pub fn open_chef(&mut self) {
        self.open_scene_pick::<kitchen::Kitchen>();
    }
}
