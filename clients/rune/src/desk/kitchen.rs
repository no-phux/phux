//! The kitchen view's state and what it does: the fleet as chefs, kept in
//! step each frame, and the way from a chef to its terminal. The scene is
//! `crate::kitchen`; the painter is `ui::kitchen`.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use crate::kitchen::{self, Cook};
use crate::machines::LOCAL;
use crate::reef::{self, Mood};

/// How often the kitchen redraws while open: every frame.
const FRAME: Duration = Duration::from_millis(16);

#[derive(Debug, Default)]
pub struct Kitchen {
    pub open: bool,
    pub scene: kitchen::Kitchen,
    /// The view's size in points, from its last layout.
    pub viewport: [f32; 2],
    /// App clock seconds at the last frame.
    last: Option<f64>,
    /// Whether the theme is dark, as the last view build saw it: night.
    pub night: Cell<bool>,
    /// Reduced motion, as the last view build saw it.
    pub still: Cell<bool>,
    /// The room's static paint and the layout generation it was made for.
    pub room: RefCell<(u32, rune::Shape)>,
    /// The night's darkness, redrawn each frame while it's dark.
    pub mask: rune::Pixels,
}

impl Desk {
    pub fn toggle_kitchen(&mut self) {
        if self.kitchen.open {
            self.close_kitchen();
            return;
        }
        self.close_inbox();
        self.close_galaxy();
        self.close_reef();
        self.close_pitch();
        self.settings_open = false;
        self.kitchen.open = true;
        self.kitchen.last = None;
        self.kitchen.scene.selected = None;
    }

    pub fn close_kitchen(&mut self) {
        self.kitchen.open = false;
    }

    /// Keep the chefs in step with the fleet and the scene moving while open.
    pub(super) fn tick_kitchen(&mut self, tx: &mut Tx) -> bool {
        if !self.kitchen.open {
            return false;
        }
        self.refresh_kitchen();
        let dt = self
            .kitchen
            .last
            .map_or(0.0, |last| (self.clock - last) as f32);
        self.kitchen.last = Some(self.clock);
        self.kitchen.scene.advance(dt);
        tx.wake_after(FRAME);
        true
    }

    /// The view laid out at `w` by `h`.
    pub fn kitchen_laid_out(&mut self, w: f32, h: f32) {
        self.kitchen.viewport = [w, h];
        self.refresh_kitchen();
    }

    /// Machines as lines in a fixed order (this one, this computer, then the
    /// registry's), so a line never swaps places when an agent there starts
    /// needing you; every agent the galaxy knows of as a chef.
    fn refresh_kitchen(&mut self) {
        let mut planets = self.planets();
        let here = self.machines.current.as_deref().unwrap_or(LOCAL);
        let hosts = &self.machines.hosts;
        planets.sort_by_key(|p| match p.name.as_str() {
            n if n == here => 0,
            LOCAL => 1,
            n => {
                2 + hosts
                    .iter()
                    .position(|h| h.name == n)
                    .unwrap_or(hosts.len())
            }
        });
        let lines: Vec<(String, bool)> =
            planets.iter().map(|p| (p.name.clone(), p.known)).collect();
        let cooks = planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| {
                p.stars.iter().map(move |s| Cook {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    host: at,
                    mood: Mood::of(s.mark),
                    species: reef::species(s.kind.as_deref(), &s.name),
                    work: s.work,
                })
            })
            .collect();
        let scene = &mut self.kitchen.scene;
        scene.nt_target = if self.kitchen.night.get() { 1.0 } else { 0.0 };
        scene.reduce = self.kitchen.still.get();
        let [w, h] = self.kitchen.viewport;
        scene.layout(w, h, &lines);
        scene.sync(cooks);
    }

    /// A press on a chef selects it; a press on the selected one opens its
    /// terminal, as the galaxy opens a star.
    pub fn kitchen_press(&mut self, id: ResourceId) {
        if self.kitchen.scene.selected.as_ref() == Some(&id) {
            self.open_chef();
        } else {
            self.kitchen.scene.selected = Some(id);
        }
    }

    pub fn kitchen_deselect(&mut self) {
        self.kitchen.scene.selected = None;
    }

    pub fn kitchen_hover(&mut self, id: ResourceId, on: bool) {
        let hovered = &mut self.kitchen.scene.hovered;
        if on {
            *hovered = Some(id);
        } else if hovered.as_ref() == Some(&id) {
            *hovered = None;
        }
    }

    /// The waiting chef after the selected one, round again at the end.
    pub fn kitchen_next_waiting(&mut self) {
        let scene = &self.kitchen.scene;
        let waiting: Vec<&ResourceId> = scene
            .chefs
            .iter()
            .filter(|c| c.who.mood == Mood::Waiting)
            .map(|c| &c.who.id)
            .collect();
        if waiting.is_empty() {
            return;
        }
        let next = waiting
            .iter()
            .position(|id| Some(*id) == scene.selected.as_ref())
            .map_or(0, |at| (at + 1) % waiting.len());
        self.kitchen.scene.selected = Some(waiting[next].clone());
    }

    /// Escape lets go of a chef first, then closes.
    pub fn kitchen_escape(&mut self) {
        if self.kitchen.scene.selected.take().is_none() {
            self.close_kitchen();
        }
    }

    /// Close the kitchen on the selected chef's terminal.
    pub fn open_chef(&mut self) {
        let Some(c) = self.kitchen.scene.selected_chef() else {
            return;
        };
        let id = c.who.id.clone();
        self.close_kitchen();
        self.go_to_star(&id);
    }
}
