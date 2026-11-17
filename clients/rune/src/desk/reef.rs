//! The reef view's state and what it does: the fleet as fish, kept in step
//! each frame, and the way from a fish to its terminal. The scene is
//! `crate::reef`; the painter is `ui::reef`. The shared ways in and out are
//! `desk::scene_host`.

use phux_protocol::ids::ResourceId;

use super::Desk;
use super::fleet::{Agent, Machine};
use super::scene_host::{FleetScene, SceneHost, SceneState};
use super::surfaces::Playground;
use crate::galaxy::{self, Kin};
use crate::playground::Mood;
use crate::reef::{self, Swimmer};

pub type Reef = SceneHost<reef::Reef>;

/// Machines as reefs in the galaxy's order, every agent it knows of as a
/// fish; schools are the galaxy's related agents. Its backdrop is the
/// caustic tile the sand's light is drawn with, made once.
impl FleetScene for reef::Reef {
    type Who = Swimmer;
    type Backdrop = rune::Pixels;
    const PLAYGROUND: Playground = Playground::Reef;
    const MACHINE_ORDER: bool = false;
    const LAYOUT_FIRST: bool = false;

    fn host(desk: &mut Desk) -> &mut Reef {
        &mut desk.reef
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
        let waiting = self.fish.iter().filter(|f| f.who.mood == Mood::Waiting);
        waiting.map(|f| f.who.id.clone()).collect()
    }

    fn selected_id(&self) -> Option<ResourceId> {
        self.selected_fish().map(|f| f.who.id.clone())
    }

    fn creatures(planets: &[Machine]) -> Vec<Swimmer> {
        let stars: Vec<(usize, &Agent)> = planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| p.agents.iter().map(move |s| (at, s)))
            .collect();
        let kin: Vec<Kin<'_>> = stars
            .iter()
            .map(|(at, s)| Kin {
                host: &planets[*at].name,
                session: &s.session,
                project: s.project.as_deref(),
            })
            .collect();
        // A school swims in one reef: related agents on two machines are
        // linked in the galaxy, not herded into one machine's water.
        let pairs: Vec<(usize, usize)> = galaxy::related(&kin)
            .into_iter()
            .filter(|&(a, b)| stars[a].0 == stars[b].0)
            .collect();
        let schools = schools(stars.len(), &pairs);
        // A school's lane is keyed like the prototype's, host/project, from
        // its first agent (its session when it has no project).
        let lanes: Vec<f32> = stars
            .iter()
            .map(|(at, s)| {
                let what = s.project.as_deref().unwrap_or(&s.session);
                reef::lane(&format!("{}/{what}", planets[*at].name))
            })
            .collect();
        stars
            .iter()
            .zip(schools)
            .map(|((at, s), school)| Swimmer {
                id: s.id.clone(),
                session: s.session.clone(),
                name: s.name.clone(),
                host: *at,
                mood: Mood::of(s.mark),
                species: reef::species(s.kind.as_deref(), &s.name),
                project: s.project.clone(),
                school,
                lane: lanes[school],
                work: s.work,
            })
            .collect()
    }

    fn layout(&mut self, w: f32, h: f32, hosts: &[(String, bool)]) {
        reef::Reef::layout(self, w, h, hosts);
    }

    fn sync(&mut self, who: Vec<Swimmer>) {
        reef::Reef::sync(self, who);
    }

    fn advance(&mut self, dt: f32) {
        reef::Reef::advance(self, dt);
    }

    fn on_toggle(caustic: &mut rune::Pixels) {
        if caustic.size() == (0, 0) {
            caustic.set(reef::TILE, reef::TILE, reef::caustic_tile());
        }
    }
}

impl Desk {
    pub fn toggle_reef(&mut self) {
        self.toggle_scene::<reef::Reef>();
    }

    pub fn close_reef(&mut self) {
        self.close_scene::<reef::Reef>();
    }

    pub fn reef_laid_out(&mut self, w: f32, h: f32) {
        self.scene_laid_out::<reef::Reef>(w, h);
    }

    pub fn reef_press(&mut self, id: ResourceId) {
        self.scene_press::<reef::Reef>(id);
    }

    pub fn reef_deselect(&mut self) {
        self.scene_deselect::<reef::Reef>();
    }

    pub fn reef_hover(&mut self, id: ResourceId, on: bool) {
        self.scene_hover::<reef::Reef>(id, on);
    }

    pub fn reef_next_waiting(&mut self) {
        self.scene_next_waiting::<reef::Reef>();
    }

    pub fn reef_escape(&mut self) {
        self.scene_escape::<reef::Reef>();
    }

    /// Close the reef on the selected fish's terminal.
    pub fn open_fish(&mut self) {
        self.open_scene_pick::<reef::Reef>();
    }
}

/// A school per connected group of `pairs` over `n` agents.
fn schools(n: usize, pairs: &[(usize, usize)]) -> Vec<usize> {
    let mut root: Vec<usize> = (0..n).collect();
    fn find(root: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    for &(a, b) in pairs {
        let (ra, rb) = (find(&mut root, a), find(&mut root, b));
        root[ra.max(rb)] = ra.min(rb);
    }
    (0..n).map(|i| find(&mut root, i)).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn related_agents_share_a_school() {
        assert_eq!(super::schools(5, &[(0, 1), (1, 3)]), vec![0, 0, 2, 0, 4]);
        assert_eq!(super::schools(3, &[(1, 2), (0, 2)]), vec![0, 0, 0]);
    }
}
