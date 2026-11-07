//! Fish grow with work done: a rise in tier while you watch is a level-up (a
//! gold ring, a glow and a burst of bubbles), and a legendary fish leaves a
//! trail of glowing motes.

use super::*;

/// One trail mote: it rises and fades.
#[derive(Clone)]
pub(crate) struct Mote {
    pub x: f32,
    pub y: f32,
    pub a: f32,
    pub c: Rgb,
}

/// Motes alive at once, across the reef.
const MOTES: usize = 240;

impl Sim {
    /// Grade `fish` again from its agent's tally at `now_ms`.
    pub(super) fn regrade(&mut self, fish: &mut Fish, now_ms: u64) {
        let grade = tier(&fish.agent, now_ms);
        if super::super::tier::rose(Some(fish.tier), grade) {
            fish.level_up = 1.;
            for _ in 0..14 {
                let s = self.s;
                let dx = (self.rand() - 0.5) * 30. * s;
                let r = (1.5 + self.rand() * 3.) * s;
                let vy = (50. + self.rand() * 40.) * s;
                self.bubble(fish.x + dx, fish.y, r, vy);
            }
        }
        fish.tier = grade;
    }

    /// A step of `dt` for growth: level-up glows fade, legendary fish drop
    /// a mote every 70 ms, and motes rise and fade over 1.6 seconds.
    pub(super) fn grow(&mut self, dt: f32) {
        let s = self.s;
        for i in 0..self.fish.len() {
            let f = &mut self.fish[i];
            f.level_up = (f.level_up - dt * 0.45).max(0.);
            if f.tier != 4 {
                continue;
            }
            f.mt -= dt;
            if f.mt > 0. || self.motes.len() >= MOTES {
                continue;
            }
            f.mt = 0.07;
            let l = f.len(s);
            let (x, y, c) = (f.x - f.face * l * 0.45, f.y, f.species.fin);
            let dy = (self.rand() - 0.5) * l * 0.2;
            self.motes.push(Mote {
                x,
                y: y + dy,
                a: 1.,
                c,
            });
        }
        self.motes.retain_mut(|m| {
            m.y -= 6. * s * dt;
            m.a -= dt * 0.6;
            m.a > 0.
        });
    }
}
