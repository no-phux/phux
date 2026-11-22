//! The pitch view's state and what it does: the fleet as players, kept in
//! step each frame, and the way from a player to its terminal. The scene is
//! `crate::pitch`; the painter is `ui::pitch`. The shared ways in and out
//! are `desk::scene_host`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use phux_protocol::ids::ResourceId;

use super::Desk;
use super::fleet::Machine;
use super::scene_host::{FleetScene, SceneHost, SceneState};
use super::surfaces::Playground;
use crate::pitch::{self, Agent, Mood, ground};
use crate::playground;

pub type Pitch = SceneHost<pitch::Pitch>;

#[derive(Debug, Default)]
pub struct Backdrop {
    /// The background by day and by night, and the layout each was painted for.
    pub ground: [rune::Pixels; 2],
    /// A cloud's shadow, made once.
    pub cloud: rune::Pixels,
    painted: [u64; 2],
    /// A background is being painted on a worker; the newest layout asked for.
    painting: Arc<AtomicBool>,
    newest: Arc<AtomicU64>,
}

impl Backdrop {
    /// Paint the day or night background the scene shows, once per layout,
    /// on a worker: a night one takes most of 100 ms, and a window being
    /// resized shows the last one stretched meanwhile. One at a time; a
    /// layout overtaken while it paints is dropped.
    fn paint_ground(&mut self, scene: &pitch::Pitch) {
        let nt = scene.nt;
        self.newest.store(scene.laid, Ordering::Relaxed);
        for (i, shown) in [nt < 0.99, nt > 0.01].into_iter().enumerate() {
            if !shown || self.painted[i] == scene.laid || self.painting.swap(true, Ordering::AcqRel)
            {
                continue;
            }
            self.painted[i] = scene.laid;
            let (scene, pixels) = (scene.clone(), self.ground[i].clone());
            let (painting, newest) = (self.painting.clone(), self.newest.clone());
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
}

/// Machines as squads in a fixed order, every agent the galaxy knows of as
/// a player.
impl FleetScene for pitch::Pitch {
    type Who = Agent;
    type Backdrop = Backdrop;
    const PLAYGROUND: Playground = Playground::Pitch;
    const MACHINE_ORDER: bool = true;
    const LAYOUT_FIRST: bool = false;

    fn host(desk: &mut Desk) -> &mut Pitch {
        &mut desk.pitch
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
        let waiting = self.players.iter().filter(|p| p.who.mood == Mood::Waiting);
        waiting.map(|p| p.who.id.clone()).collect()
    }

    fn selected_id(&self) -> Option<ResourceId> {
        self.selected_player().map(|p| p.who.id.clone())
    }

    fn creatures(planets: &[Machine]) -> Vec<Agent> {
        planets
            .iter()
            .enumerate()
            .flat_map(|(at, p)| p.agents.iter().map(move |s| (at, s)))
            .map(|(at, s)| Agent {
                id: s.id.clone(),
                name: s.name.clone(),
                host: at,
                mood: Mood::of(s.mark),
                species: playground::species(s.kind.as_deref(), &s.name),
                number: s
                    .terminal
                    .local_id()
                    .unwrap_or_else(|| playground::hash(&s.name) % 98 + 1),
                work: s.work,
            })
            .collect()
    }

    fn layout(&mut self, w: f32, h: f32, hosts: &[(String, bool)]) {
        pitch::Pitch::layout(self, w, h, hosts);
    }

    fn sync(&mut self, who: Vec<Agent>) {
        pitch::Pitch::sync(self, who);
    }

    fn advance(&mut self, dt: f32) {
        pitch::Pitch::advance(self, dt);
    }

    fn on_toggle(backdrop: &mut Backdrop) {
        if backdrop.cloud.size() == (0, 0) {
            backdrop
                .cloud
                .set(ground::GLOW, ground::GLOW, ground::glow_tile());
        }
    }

    fn after_advance(&self, backdrop: &mut Backdrop) {
        backdrop.paint_ground(self);
    }
}

impl Desk {
    pub fn toggle_pitch(&mut self) {
        self.toggle_scene::<pitch::Pitch>();
    }

    pub fn close_pitch(&mut self) {
        self.close_scene::<pitch::Pitch>();
    }

    pub fn pitch_laid_out(&mut self, w: f32, h: f32) {
        self.scene_laid_out::<pitch::Pitch>(w, h);
    }

    pub fn pitch_press(&mut self, id: ResourceId) {
        self.scene_press::<pitch::Pitch>(id);
    }

    pub fn pitch_deselect(&mut self) {
        self.scene_deselect::<pitch::Pitch>();
    }

    pub fn pitch_hover(&mut self, id: ResourceId, on: bool) {
        self.scene_hover::<pitch::Pitch>(id, on);
    }

    pub fn pitch_next_waiting(&mut self) {
        self.scene_next_waiting::<pitch::Pitch>();
    }

    pub fn pitch_escape(&mut self) {
        self.scene_escape::<pitch::Pitch>();
    }

    /// Close the pitch on the selected player's terminal.
    pub fn open_player(&mut self) {
        self.open_scene_pick::<pitch::Pitch>();
    }
}
