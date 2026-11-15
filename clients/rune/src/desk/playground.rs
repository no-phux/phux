//! What the reef, pitch, kitchen, pit lane and hive share: being open, the
//! scene kept in step with the fleet each frame, and a pick that leads to
//! its agent's terminal. Each scene is `crate::<name>`, its painter
//! `ui::<name>`; the galaxy is its own.

use std::cell::Cell;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use super::galaxy::Planet;
use crate::creature::{self, Agent};
use crate::machines::LOCAL;
use crate::reef::{self, Mood};

/// How often a playground redraws while open: every frame.
const FRAME: Duration = Duration::from_millis(16);

#[derive(Debug, Default)]
pub struct Playground<S, P = ()> {
    pub open: bool,
    pub scene: S,
    /// The view's size in points, from its last layout.
    pub viewport: [f32; 2],
    /// App clock seconds at the last frame.
    last: Option<f64>,
    /// Whether the theme is dark, as the last view build saw it: night.
    pub night: Cell<bool>,
    /// Reduced motion, as the last view build saw it.
    pub still: Cell<bool>,
    /// What the painter keeps between frames.
    pub paint: P,
}

/// A scene the fleet plays out in.
pub trait Scene: Sized + 'static {
    type Paint;
    fn of(desk: &mut Desk) -> &mut Playground<Self, Self::Paint>;
    /// The scene in step with the fleet, at its viewport.
    fn refresh(desk: &mut Desk);
    /// After each refresh and frame.
    fn settle(_desk: &mut Desk) {}
    /// On opening.
    fn opened(_desk: &mut Desk) {}
    /// Each creature's agent and mood, in the scene's order.
    fn agents(&self) -> impl Iterator<Item = (&ResourceId, Mood)>;
    fn pick(&mut self) -> &mut Option<ResourceId>;
    fn hover(&mut self) -> &mut Option<ResourceId>;
    fn look(&mut self, night: bool, still: bool);
    fn play(&mut self, dt: f32);
}

/// The [`Scene`] methods every model has alike, over its creatures' field.
macro_rules! creatures {
    ($field:ident) => {
        fn agents(
            &self,
        ) -> impl Iterator<Item = (&phux_protocol::ids::ResourceId, crate::reef::Mood)> {
            self.$field.iter().map(|c| (&c.who.id, c.who.mood))
        }
        fn pick(&mut self) -> &mut Option<phux_protocol::ids::ResourceId> {
            &mut self.selected
        }
        fn hover(&mut self) -> &mut Option<phux_protocol::ids::ResourceId> {
            &mut self.hovered
        }
        fn look(&mut self, night: bool, still: bool) {
            self.nt_target = if night { 1.0 } else { 0.0 };
            self.reduce = still;
        }
        fn play(&mut self, dt: f32) {
            self.advance(dt);
        }
    };
}
pub(super) use creatures;

impl Desk {
    pub fn toggle_scene<S: Scene>(&mut self) {
        if S::of(self).open {
            self.close_scene::<S>();
            return;
        }
        self.close_inbox();
        self.close_galaxy();
        self.close_scenes();
        self.settings_open = false;
        let v = S::of(self);
        v.open = true;
        v.last = None;
        *v.scene.pick() = None;
        S::opened(self);
    }

    pub fn close_scene<S: Scene>(&mut self) {
        S::of(self).open = false;
    }

    pub(super) fn close_scenes(&mut self) {
        self.reef.open = false;
        self.pitch.open = false;
        self.kitchen.open = false;
        self.pitlane.open = false;
        self.hive.open = false;
    }

    pub(super) fn scene_open(&self) -> bool {
        self.reef.open
            || self.pitch.open
            || self.kitchen.open
            || self.pitlane.open
            || self.hive.open
    }

    /// Keep the creatures in step with the fleet and the scene moving while open.
    pub(super) fn tick_scene<S: Scene>(&mut self, tx: &mut Tx) -> bool {
        if !S::of(self).open {
            return false;
        }
        self.refresh_scene::<S>();
        let clock = self.clock;
        let v = S::of(self);
        let dt = v.last.map_or(0.0, |last| (clock - last) as f32);
        v.last = Some(clock);
        v.scene.play(dt);
        S::settle(self);
        tx.wake_after(FRAME);
        true
    }

    /// The view laid out at `w` by `h`.
    pub fn scene_laid_out<S: Scene>(&mut self, w: f32, h: f32) {
        S::of(self).viewport = [w, h];
        self.refresh_scene::<S>();
        S::settle(self);
    }

    fn refresh_scene<S: Scene>(&mut self) {
        let v = S::of(self);
        let (night, still) = (v.night.get(), v.still.get());
        v.scene.look(night, still);
        S::refresh(self);
    }

    /// A press on a creature selects it; a press on the selected one opens
    /// its terminal, as the galaxy opens a star.
    pub fn scene_press<S: Scene>(&mut self, id: ResourceId) {
        let pick = S::of(self).scene.pick();
        if pick.as_ref() == Some(&id) {
            self.open_agent::<S>();
        } else {
            *pick = Some(id);
        }
    }

    pub fn scene_deselect<S: Scene>(&mut self) {
        *S::of(self).scene.pick() = None;
    }

    pub fn scene_hover<S: Scene>(&mut self, id: ResourceId, on: bool) {
        let hovered = S::of(self).scene.hover();
        if on {
            *hovered = Some(id);
        } else if hovered.as_ref() == Some(&id) {
            *hovered = None;
        }
    }

    /// The waiting creature after the selected one, round again at the end.
    pub fn scene_next_waiting<S: Scene>(&mut self) {
        let scene = &mut S::of(self).scene;
        let waiting: Vec<ResourceId> = scene
            .agents()
            .filter(|(_, mood)| *mood == Mood::Waiting)
            .map(|(id, _)| id.clone())
            .collect();
        if waiting.is_empty() {
            return;
        }
        let pick = scene.pick();
        let next = waiting
            .iter()
            .position(|id| Some(id) == pick.as_ref())
            .map_or(0, |at| (at + 1) % waiting.len());
        *pick = Some(waiting[next].clone());
    }

    /// Escape lets go of a creature first, then closes.
    pub fn scene_escape<S: Scene>(&mut self) {
        if S::of(self).scene.pick().take().is_none() {
            self.close_scene::<S>();
        }
    }

    /// Close the scene on the selected creature's terminal.
    pub fn open_agent<S: Scene>(&mut self) {
        let scene = &mut S::of(self).scene;
        let picked = scene.pick().clone();
        let Some(id) = picked.filter(|id| scene.agents().any(|(a, _)| a == id)) else {
            return;
        };
        self.close_scene::<S>();
        self.go_to_star(&id);
    }

    /// Every machine (name, agents known) and every agent it knows of.
    /// `steady` keeps machines in a fixed order (this one, this computer,
    /// then the registry's), so none swaps places when an agent there starts
    /// needing you; otherwise they come in the galaxy's needs-you-first order.
    pub(super) fn cast(&self, steady: bool) -> (Vec<(String, bool)>, Vec<Agent>) {
        let mut planets = self.planets();
        if steady {
            let hosts: Vec<String> = self.machines.hosts.iter().map(|h| h.name.clone()).collect();
            in_machine_order(&mut planets, self.machines.current.as_deref(), &hosts);
        }
        let hosts = planets.iter().map(|p| (p.name.clone(), p.known)).collect();
        let agents = planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| p.stars.iter().map(move |s| (at, s)))
            .map(|(at, s)| Agent {
                id: s.id.clone(),
                name: s.name.clone(),
                host: at,
                mood: Mood::of(s.mark),
                species: reef::species(s.kind.as_deref(), &s.name),
                tier: creature::tier(s.work.as_ref()),
                number: s
                    .terminal
                    .local_id()
                    .unwrap_or_else(|| reef::hash(&s.name) % 98 + 1),
                work: s.work,
            })
            .collect();
        (hosts, agents)
    }
}

/// The connected machine, this computer, then the paired hosts; the rest
/// after, in the order they came.
fn in_machine_order(planets: &mut [Planet], current: Option<&str>, hosts: &[String]) {
    let order: Vec<&str> = std::iter::once(current.unwrap_or(LOCAL))
        .chain(std::iter::once(LOCAL))
        .chain(hosts.iter().map(String::as_str))
        .collect();
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
    fn machines_keep_their_order_when_one_needs_you() {
        let hosts = ["macbook".to_owned(), "powerhouse-1".to_owned()];
        // Needs-you-first, as the galaxy hands them over.
        let mut planets = vec![planet("powerhouse-1"), planet(LOCAL), planet("macbook")];
        in_machine_order(&mut planets, None, &hosts);
        let names: Vec<&str> = planets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, [LOCAL, "macbook", "powerhouse-1"]);
        let mut planets = vec![planet(LOCAL), planet("powerhouse-1"), planet("macbook")];
        in_machine_order(&mut planets, Some("powerhouse-1"), &hosts);
        let names: Vec<&str> = planets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["powerhouse-1", LOCAL, "macbook"]);
    }
}
