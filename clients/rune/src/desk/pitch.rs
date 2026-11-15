//! The pitch view: the fleet as players, machines as squads in a steady order.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::Desk;
use super::playground::{Playground, Scene, creatures};
use crate::pitch::{self, ground};

pub type Pitch = Playground<pitch::Pitch, Paint>;

#[derive(Debug, Default)]
pub struct Paint {
    /// The background by day and by night, and the layout each was painted for.
    pub ground: [rune::Pixels; 2],
    /// A cloud's shadow, made once.
    pub cloud: rune::Pixels,
    painted: [u64; 2],
    /// A background is being painted on a worker; the newest layout asked for.
    painting: Arc<AtomicBool>,
    newest: Arc<AtomicU64>,
}

impl Scene for pitch::Pitch {
    type Paint = Paint;

    fn of(desk: &mut Desk) -> &mut Pitch {
        &mut desk.pitch
    }

    fn opened(desk: &mut Desk) {
        let cloud = &desk.pitch.paint.cloud;
        if cloud.size() == (0, 0) {
            cloud.set(ground::GLOW, ground::GLOW, ground::glow_tile());
        }
    }

    fn refresh(desk: &mut Desk) {
        let (hosts, agents) = desk.cast(true);
        let v = &mut desk.pitch;
        v.scene.sync(agents);
        let [w, h] = v.viewport;
        v.scene.layout(w, h, &hosts);
    }

    /// Paint the day or night background the scene shows, once per layout,
    /// on a worker: a night one takes most of 100 ms, and a window being
    /// resized shows the last one stretched meanwhile. One at a time; a
    /// layout overtaken while it paints is dropped.
    fn settle(desk: &mut Desk) {
        let (scene, p) = (&desk.pitch.scene, &mut desk.pitch.paint);
        let nt = scene.nt;
        p.newest.store(scene.laid, Ordering::Relaxed);
        for (i, shown) in [nt < 0.99, nt > 0.01].into_iter().enumerate() {
            if !shown || p.painted[i] == scene.laid || p.painting.swap(true, Ordering::AcqRel) {
                continue;
            }
            p.painted[i] = scene.laid;
            let (scene, pixels) = (scene.clone(), p.ground[i].clone());
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

    creatures!(players);
}
