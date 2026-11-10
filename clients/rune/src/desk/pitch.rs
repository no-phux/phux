//! The pitch view's state and what it does: the fleet as players, kept in
//! step each frame, and the way from a player to its terminal. The scene is
//! `crate::pitch`; the painter is `ui::pitch`.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use super::galaxy::Planet;
use crate::machines::LOCAL;
use crate::pitch::{self, Agent, Mood, ground};
use crate::reef;

/// How often the pitch redraws while open: every frame.
const FRAME: Duration = Duration::from_millis(16);

#[derive(Debug, Default)]
pub struct Pitch {
    pub open: bool,
    pub scene: pitch::Pitch,
    /// The view's size in points, from its last layout.
    pub viewport: [f32; 2],
    /// App clock seconds at the last frame.
    last: Option<f64>,
    /// Whether the theme is dark, as the last view build saw it: night.
    pub night: Cell<bool>,
    /// Reduced motion, as the last view build saw it.
    pub still: Cell<bool>,
    /// The background by day and by night, and the layout each was painted for.
    pub ground: [rune::Pixels; 2],
    /// A cloud's shadow, made once.
    pub cloud: rune::Pixels,
    painted: [u64; 2],
    /// A background is being painted on a worker; the newest layout asked for.
    painting: Arc<AtomicBool>,
    newest: Arc<AtomicU64>,
}

impl Desk {
    pub fn toggle_pitch(&mut self) {
        if self.pitch.open {
            self.close_pitch();
            return;
        }
        self.close_inbox();
        self.close_galaxy();
        self.close_reef();
        self.close_kitchen();
        self.close_pitlane();
        self.close_hive();
        self.settings_open = false;
        self.pitch.open = true;
        self.pitch.last = None;
        self.pitch.scene.selected = None;
        if self.pitch.cloud.size() == (0, 0) {
            self.pitch
                .cloud
                .set(ground::GLOW, ground::GLOW, ground::glow_tile());
        }
    }

    pub fn close_pitch(&mut self) {
        self.pitch.open = false;
    }

    /// Keep the players in step with the fleet and the scene moving while open.
    pub(super) fn tick_pitch(&mut self, tx: &mut Tx) -> bool {
        if !self.pitch.open {
            return false;
        }
        self.refresh_pitch();
        let dt = self
            .pitch
            .last
            .map_or(0.0, |last| (self.clock - last) as f32);
        self.pitch.last = Some(self.clock);
        self.pitch.scene.advance(dt);
        self.paint_ground();
        tx.wake_after(FRAME);
        true
    }

    /// The view laid out at `w` by `h`.
    pub fn pitch_laid_out(&mut self, w: f32, h: f32) {
        self.pitch.viewport = [w, h];
        self.refresh_pitch();
        self.paint_ground();
    }

    /// Paint the day or night background the scene shows, once per layout,
    /// on a worker: a night one takes most of 100 ms, and a window being
    /// resized shows the last one stretched meanwhile. One at a time; a
    /// layout overtaken while it paints is dropped.
    fn paint_ground(&mut self) {
        let p = &mut self.pitch;
        let nt = p.scene.nt;
        p.newest.store(p.scene.laid, Ordering::Relaxed);
        for (i, shown) in [nt < 0.99, nt > 0.01].into_iter().enumerate() {
            if !shown || p.painted[i] == p.scene.laid || p.painting.swap(true, Ordering::AcqRel) {
                continue;
            }
            p.painted[i] = p.scene.laid;
            let (scene, pixels) = (p.scene.clone(), p.ground[i].clone());
            let (painting, newest) = (p.painting.clone(), p.newest.clone());
            std::thread::spawn(move || {
                let image = ground::render(&scene, i == 1);
                if let Some((w, h, rgba)) =
                    image.filter(|_| newest.load(Ordering::Relaxed) == scene.laid)
                {
                    pixels.set(w, h, rgba);
                }
                painting.store(false, Ordering::Release);
            });
        }
    }

    /// Machines as squads in the galaxy's order, every agent it knows of as
    /// a player.
    fn refresh_pitch(&mut self) {
        let mut planets = self.planets();
        in_machine_order(
            &mut planets,
            self.machines.current.as_deref(),
            &self.host_names(),
        );
        let hosts: Vec<(String, bool)> =
            planets.iter().map(|p| (p.name.clone(), p.known)).collect();
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
                number: s
                    .terminal
                    .local_id()
                    .unwrap_or_else(|| reef::hash(&s.name) % 98 + 1),
                work: s.work,
            })
            .collect();
        let scene = &mut self.pitch.scene;
        scene.nt_target = if self.pitch.night.get() { 1.0 } else { 0.0 };
        scene.reduce = self.pitch.still.get();
        scene.sync(agents);
        let [w, h] = self.pitch.viewport;
        scene.layout(w, h, &hosts);
    }

    fn host_names(&self) -> Vec<String> {
        self.machines.hosts.iter().map(|h| h.name.clone()).collect()
    }

    /// A press on a player selects it; a press on the selected one opens its
    /// terminal, as the galaxy opens a star.
    pub fn pitch_press(&mut self, id: ResourceId) {
        if self.pitch.scene.selected.as_ref() == Some(&id) {
            self.open_player();
        } else {
            self.pitch.scene.selected = Some(id);
        }
    }

    pub fn pitch_deselect(&mut self) {
        self.pitch.scene.selected = None;
    }

    pub fn pitch_hover(&mut self, id: ResourceId, on: bool) {
        let hovered = &mut self.pitch.scene.hovered;
        if on {
            *hovered = Some(id);
        } else if hovered.as_ref() == Some(&id) {
            *hovered = None;
        }
    }

    /// The waiting player after the selected one, round again at the end.
    pub fn pitch_next_waiting(&mut self) {
        let scene = &self.pitch.scene;
        let waiting: Vec<ResourceId> = scene
            .players
            .iter()
            .filter(|p| p.who.mood == Mood::Waiting)
            .map(|p| p.who.id.clone())
            .collect();
        if waiting.is_empty() {
            return;
        }
        let next = waiting
            .iter()
            .position(|id| Some(id) == scene.selected.as_ref())
            .map_or(0, |at| (at + 1) % waiting.len());
        self.pitch.scene.selected = Some(waiting[next].clone());
    }

    /// Escape lets go of a player first, then closes.
    pub fn pitch_escape(&mut self) {
        if self.pitch.scene.selected.take().is_none() {
            self.close_pitch();
        }
    }

    /// Close the pitch on the selected player's terminal.
    pub fn open_player(&mut self) {
        let Some(p) = self.pitch.scene.selected_player() else {
            return;
        };
        let id = p.who.id.clone();
        self.close_pitch();
        self.go_to_star(&id);
    }
}

/// The galaxy lists machines needing you first; a squad keeps its third
/// instead: the connected machine, this computer, then the paired hosts.
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
    fn squads_keep_their_third_when_a_machine_needs_you() {
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
