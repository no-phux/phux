//! The reef view: the fleet as fish, machines as reefs in the galaxy's order.

use super::Desk;
use super::galaxy::Star;
use super::playground::{Playground, Scene, creatures};
use crate::galaxy::{self, Kin};
use crate::reef::{self, Mood, Swimmer};

/// Its paint: the caustic tile the sand's light is drawn with, made once.
pub type Reef = Playground<reef::Reef, rune::Pixels>;

impl Scene for reef::Reef {
    type Paint = rune::Pixels;

    fn of(desk: &mut Desk) -> &mut Reef {
        &mut desk.reef
    }

    fn opened(desk: &mut Desk) {
        let caustic = &desk.reef.paint;
        if caustic.size() == (0, 0) {
            caustic.set(reef::TILE, reef::TILE, reef::caustic_tile());
        }
    }

    /// Schools are the galaxy's related agents.
    fn refresh(desk: &mut Desk) {
        let planets = desk.planets();
        let hosts: Vec<(String, bool)> =
            planets.iter().map(|p| (p.name.clone(), p.known)).collect();
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
        let swimmers = stars
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
            .collect();
        let v = &mut desk.reef;
        v.scene.sync(swimmers);
        let [w, h] = v.viewport;
        v.scene.layout(w, h, &hosts);
    }

    creatures!(fish);
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
