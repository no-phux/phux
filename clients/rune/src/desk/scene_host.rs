//! What the fleet scenes (hive, kitchen, pitch, pit lane, reef) share: the
//! view's state around its simulation, and the Desk's ways in and out of
//! it, written once. Each sim says what differs through [`FleetScene`]. The
//! galaxy has its own camera and clock and stays apart.

use std::cell::Cell;
use std::fmt::Debug;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use super::fleet::Machine;
use super::surfaces::Playground;
use crate::machines::LOCAL;

/// How often a fleet scene redraws while open: every frame.
const FRAME: Duration = Duration::from_millis(16);

/// The fields of a sim the shared Desk code reads and writes.
pub struct SceneState<'a> {
    /// The picked agent, whether or not it is still in the scene.
    pub selected: &'a mut Option<ResourceId>,
    pub hovered: &'a mut Option<ResourceId>,
    /// Where night is heading: 1 dark, 0 light.
    pub nt_target: &'a mut f32,
    /// Reduced motion.
    pub reduce: &'a mut bool,
}

/// A fleet scene's simulation, as its view on the Desk drives it.
pub trait FleetScene: Default + Debug + Sized {
    /// One agent as the scene's `sync` takes it.
    type Who;
    /// What the view keeps painted between frames; it differs by scene.
    type Backdrop: Default + Debug;
    const PLAYGROUND: Playground;
    /// Machines in a fixed order (connected, this computer, the registry's)
    /// rather than the galaxy's needs-you-first one.
    const MACHINE_ORDER: bool;
    /// Lay out before syncing the agents, rather than after.
    const LAYOUT_FIRST: bool;

    /// The scene's view on the Desk.
    fn host(desk: &mut Desk) -> &mut SceneHost<Self>;
    fn state(&mut self) -> SceneState<'_>;
    /// The agents waiting on you, in the scene's order.
    fn waiting_ids(&self) -> Vec<ResourceId>;
    /// The picked agent, when it is still in the scene.
    fn selected_id(&self) -> Option<ResourceId>;
    /// Every agent on `planets`, by its machine's index.
    fn creatures(planets: &[Machine]) -> Vec<Self::Who>;
    fn layout(&mut self, w: f32, h: f32, hosts: &[(String, bool)]);
    fn sync(&mut self, who: Vec<Self::Who>);
    fn advance(&mut self, dt: f32);

    /// The view opening: make what is made once.
    fn on_toggle(_backdrop: &mut Self::Backdrop) {}
    /// After each refresh and step.
    fn after_advance(&self, _backdrop: &mut Self::Backdrop) {}
}

/// A fleet scene's view: its sim and what the view keeps around it.
#[derive(Debug, Default)]
pub struct SceneHost<S: FleetScene> {
    pub scene: S,
    /// The view's size in points, from its last layout.
    pub viewport: [f32; 2],
    /// App clock seconds at the last frame.
    last: Option<f64>,
    /// Whether the theme is dark, as the last view build saw it: night.
    pub night: Cell<bool>,
    /// Reduced motion, as the last view build saw it.
    pub still: Cell<bool>,
    pub backdrop: S::Backdrop,
}

impl Desk {
    pub(super) fn toggle_scene<S: FleetScene>(&mut self) {
        if self.playground == Some(S::PLAYGROUND) {
            self.close_scene::<S>();
            return;
        }
        self.close_inbox();
        self.settings_open = false;
        self.playground = Some(S::PLAYGROUND);
        let host = S::host(self);
        host.last = None;
        *host.scene.state().selected = None;
        S::on_toggle(&mut host.backdrop);
    }

    pub(super) fn close_scene<S: FleetScene>(&mut self) {
        self.close_playground(S::PLAYGROUND);
    }

    /// Keep the agents in step with the fleet and the scene moving while open.
    pub(super) fn tick_scene<S: FleetScene>(&mut self, tx: &mut Tx) -> bool {
        if self.playground != Some(S::PLAYGROUND) {
            return false;
        }
        self.refresh_scene::<S>();
        let clock = self.clock;
        let host = S::host(self);
        let dt = host.last.map_or(0.0, |last| (clock - last) as f32);
        host.last = Some(clock);
        host.scene.advance(dt);
        host.scene.after_advance(&mut host.backdrop);
        tx.wake_after(FRAME);
        true
    }

    /// The view laid out at `w` by `h`.
    pub(super) fn scene_laid_out<S: FleetScene>(&mut self, w: f32, h: f32) {
        S::host(self).viewport = [w, h];
        self.refresh_scene::<S>();
        let host = S::host(self);
        host.scene.after_advance(&mut host.backdrop);
    }

    /// Machines and every agent the galaxy knows of, into the scene.
    fn refresh_scene<S: FleetScene>(&mut self) {
        let mut planets = self.fleet_machines();
        if S::MACHINE_ORDER {
            fleet_in_machine_order(
                &mut planets,
                self.machines.current.as_deref(),
                &self.host_names(),
            );
        }
        let hosts: Vec<(String, bool)> =
            planets.iter().map(|p| (p.name.clone(), p.known)).collect();
        let who = S::creatures(&planets);
        let host = S::host(self);
        let state = host.scene.state();
        *state.nt_target = if host.night.get() { 1.0 } else { 0.0 };
        *state.reduce = host.still.get();
        let [w, h] = host.viewport;
        let scene = &mut host.scene;
        if S::LAYOUT_FIRST {
            scene.layout(w, h, &hosts);
            scene.sync(who);
        } else {
            scene.sync(who);
            scene.layout(w, h, &hosts);
        }
    }

    /// A press on an agent selects it; a press on the selected one opens its
    /// terminal, as the galaxy opens a star.
    pub(super) fn scene_press<S: FleetScene>(&mut self, id: ResourceId) {
        let selected = S::host(self).scene.state().selected;
        if selected.as_ref() == Some(&id) {
            self.open_scene_pick::<S>();
        } else {
            *selected = Some(id);
        }
    }

    pub(super) fn scene_deselect<S: FleetScene>(&mut self) {
        *S::host(self).scene.state().selected = None;
    }

    pub(super) fn scene_hover<S: FleetScene>(&mut self, id: ResourceId, on: bool) {
        let hovered = S::host(self).scene.state().hovered;
        if on {
            *hovered = Some(id);
        } else if hovered.as_ref() == Some(&id) {
            *hovered = None;
        }
    }

    /// The waiting agent after the selected one, round again at the end.
    pub(super) fn scene_next_waiting<S: FleetScene>(&mut self) {
        let scene = &mut S::host(self).scene;
        let mut waiting = scene.waiting_ids();
        if waiting.is_empty() {
            return;
        }
        let selected = scene.state().selected;
        let next = waiting
            .iter()
            .position(|id| Some(id) == selected.as_ref())
            .map_or(0, |at| (at + 1) % waiting.len());
        *selected = Some(waiting.swap_remove(next));
    }

    /// Escape lets go of an agent first, then closes.
    pub(super) fn scene_escape<S: FleetScene>(&mut self) {
        if S::host(self).scene.state().selected.take().is_none() {
            self.close_scene::<S>();
        }
    }

    /// Close the scene on the selected agent's terminal.
    pub(super) fn open_scene_pick<S: FleetScene>(&mut self) {
        let Some(id) = S::host(self).scene.selected_id() else {
            return;
        };
        self.close_scene::<S>();
        self.go_to_agent(&id);
    }

    /// The paired hosts' names, in the registry's order.
    fn host_names(&self) -> Vec<String> {
        self.machines.hosts.iter().map(|h| h.name.clone()).collect()
    }
}

/// The galaxy lists machines needing you first; a scene that keeps its
/// places sorts them back: the connected machine, this computer, then the
/// paired hosts, the rest after in the order they came. A stable sort, so
/// the galaxy's tie order holds.
fn fleet_in_machine_order(planets: &mut [Machine], current: Option<&str>, hosts: &[String]) {
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

    fn planet(name: &str) -> Machine {
        Machine {
            name: name.to_owned(),
            online: true,
            agents: Vec::new(),
            known: true,
            note: None,
        }
    }

    fn names(planets: &[Machine]) -> Vec<&str> {
        planets.iter().map(|p| p.name.as_str()).collect()
    }

    /// The pitch's squads keep their third when a machine needs you.
    #[test]
    fn squads_keep_their_third_when_a_machine_needs_you() {
        let hosts = ["macbook".to_owned(), "powerhouse-1".to_owned()];
        // Needs-you-first, as the galaxy hands them over.
        let mut planets = vec![planet("powerhouse-1"), planet(LOCAL), planet("macbook")];
        fleet_in_machine_order(&mut planets, None, &hosts);
        assert_eq!(names(&planets), [LOCAL, "macbook", "powerhouse-1"]);
        let mut planets = vec![planet(LOCAL), planet("powerhouse-1"), planet("macbook")];
        fleet_in_machine_order(&mut planets, Some("powerhouse-1"), &hosts);
        assert_eq!(names(&planets), ["powerhouse-1", LOCAL, "macbook"]);
    }

    /// The kitchen's lines: this one, this computer, the registry's, then
    /// machines the registry doesn't list, in the order they came.
    #[test]
    fn kitchen_lines_keep_connected_local_registry_order() {
        let hosts = ["macbook".to_owned(), "powerhouse-1".to_owned()];
        let mut planets = vec![
            planet("stray"),
            planet("powerhouse-1"),
            planet(LOCAL),
            planet("other"),
            planet("macbook"),
            planet("mini"),
        ];
        fleet_in_machine_order(&mut planets, Some("mini"), &hosts);
        assert_eq!(
            names(&planets),
            ["mini", LOCAL, "macbook", "powerhouse-1", "stray", "other"]
        );
    }
}
