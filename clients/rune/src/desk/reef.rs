//! The reef view's state and what it does: the fleet as fish, kept in step
//! each frame, and the way from a fish to its terminal. The scene is
//! `crate::reef`; the painter is `ui::reef`.

use std::cell::Cell;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use super::galaxy::Star;
use crate::galaxy::{self, Kin};
use crate::machines::LOCAL;
use crate::reef::{self, Mood, Swimmer};

/// How often the reef redraws while open: every frame.
const FRAME: Duration = Duration::from_millis(16);

#[derive(Debug, Default)]
pub struct Reef {
    pub open: bool,
    pub scene: reef::Reef,
    /// The view's size in points, from its last layout.
    pub viewport: [f32; 2],
    /// App clock seconds at the last frame.
    last: Option<f64>,
    /// Whether the theme is dark, as the last view build saw it: night.
    pub night: Cell<bool>,
    /// Reduced motion, as the last view build saw it.
    pub still: Cell<bool>,
    /// The caustic tile the sand's light is drawn with, made once.
    pub caustic: rune::Pixels,
}

impl Desk {
    pub fn toggle_reef(&mut self) {
        if self.reef.open {
            self.close_reef();
            return;
        }
        self.close_inbox();
        self.close_galaxy();
        self.settings_open = false;
        self.reef.open = true;
        self.reef.last = None;
        self.reef.scene.selected = None;
        if self.reef.caustic.size() == (0, 0) {
            self.reef
                .caustic
                .set(reef::TILE, reef::TILE, reef::caustic_tile());
        }
    }

    pub fn close_reef(&mut self) {
        self.reef.open = false;
    }

    /// Keep the fish in step with the fleet and the scene moving while open.
    pub(super) fn tick_reef(&mut self, tx: &mut Tx) -> bool {
        if !self.reef.open {
            return false;
        }
        self.refresh_reef();
        let dt = self
            .reef
            .last
            .map_or(0.0, |last| (self.clock - last) as f32);
        self.reef.last = Some(self.clock);
        self.reef.scene.advance(dt);
        tx.wake_after(FRAME);
        true
    }

    /// The view laid out at `w` by `h`.
    pub fn reef_laid_out(&mut self, w: f32, h: f32) {
        self.reef.viewport = [w, h];
        self.refresh_reef();
    }

    /// Machines as reefs in the galaxy's order, the connected one's agents as
    /// fish; schools are the galaxy's related agents.
    fn refresh_reef(&mut self) {
        let planets = self.planets();
        let here = self
            .machines
            .current
            .clone()
            .unwrap_or_else(|| LOCAL.to_owned());
        let hosts: Vec<(String, bool)> = planets
            .iter()
            .map(|p| (p.name.clone(), p.name == here))
            .collect();
        let stars: Vec<(usize, &Star)> = planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| p.stars.iter().map(move |s| (at, s)))
            .collect();
        let kin: Vec<Kin<'_>> = stars
            .iter()
            .map(|(at, s)| Kin {
                host: &planets[*at].name,
                session: &s.session,
                project: s.project.as_deref(),
            })
            .collect();
        let schools = schools(stars.len(), &galaxy::related(&kin));
        let swimmers = stars
            .iter()
            .zip(schools)
            .map(|((at, s), school)| Swimmer {
                id: s.terminal.clone(),
                session: s.session.clone(),
                name: s.name.clone(),
                host: *at,
                mood: Mood::of(s.mark),
                species: reef::species(self.activities.agent_kind(&s.terminal), &s.name),
                project: s.project.clone(),
                school,
            })
            .collect();
        let scene = &mut self.reef.scene;
        scene.nt_target = if self.reef.night.get() { 1.0 } else { 0.0 };
        scene.reduce = self.reef.still.get();
        scene.sync(swimmers);
        let [w, h] = self.reef.viewport;
        scene.layout(w, h, &hosts);
    }

    /// A press on a fish selects it; a press on the selected one opens its
    /// terminal, as the galaxy opens a star.
    pub fn reef_press(&mut self, id: ResourceId) {
        if self.reef.scene.selected.as_ref() == Some(&id) {
            self.open_fish();
        } else {
            self.reef.scene.selected = Some(id);
        }
    }

    pub fn reef_deselect(&mut self) {
        self.reef.scene.selected = None;
    }

    pub fn reef_hover(&mut self, id: ResourceId, on: bool) {
        let hovered = &mut self.reef.scene.hovered;
        if on {
            *hovered = Some(id);
        } else if hovered.as_ref() == Some(&id) {
            *hovered = None;
        }
    }

    /// The waiting fish after the selected one, round again at the end.
    pub fn reef_next_waiting(&mut self) {
        let scene = &self.reef.scene;
        let waiting: Vec<ResourceId> = scene
            .fish
            .iter()
            .filter(|f| f.who.mood == Mood::Waiting)
            .map(|f| f.who.id.clone())
            .collect();
        if waiting.is_empty() {
            return;
        }
        let next = waiting
            .iter()
            .position(|id| Some(id) == scene.selected.as_ref())
            .map_or(0, |at| (at + 1) % waiting.len());
        self.reef.scene.selected = Some(waiting[next].clone());
    }

    /// Escape lets go of a fish first, then closes.
    pub fn reef_escape(&mut self) {
        if self.reef.scene.selected.take().is_none() {
            self.close_reef();
        }
    }

    /// Close the reef on the selected fish's terminal.
    pub fn open_fish(&mut self) {
        let Some(f) = self.reef.scene.selected_fish() else {
            return;
        };
        let (terminal, session) = (f.who.id.clone(), f.who.session.clone());
        self.close_reef();
        self.show(terminal, &session);
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
