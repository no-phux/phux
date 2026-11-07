//! The scene's palette, day and night, from the prototype.

use super::{Rgb, mix};

/// The scene's colours, day to night.
#[derive(Debug, Clone, Copy)]
pub struct Pal {
    pub sky0: Rgb,
    pub sky1: Rgb,
    pub w0: Rgb,
    pub w1: Rgb,
    pub w2: Rgb,
    pub sand0: Rgb,
    pub sand1: Rgb,
    pub far: Rgb,
    pub far2: Rgb,
    pub kelp: Rgb,
    pub ink: Rgb,
}

const DAY: Pal = Pal {
    sky0: [120.0, 196.0, 232.0],
    sky1: [214.0, 238.0, 246.0],
    w0: [58.0, 186.0, 206.0],
    w1: [14.0, 108.0, 148.0],
    w2: [6.0, 40.0, 70.0],
    sand0: [222.0, 202.0, 156.0],
    sand1: [128.0, 112.0, 86.0],
    far: [26.0, 112.0, 146.0],
    far2: [40.0, 140.0, 168.0],
    kelp: [72.0, 132.0, 70.0],
    ink: [4.0, 22.0, 36.0],
};
const NIGHT: Pal = Pal {
    sky0: [4.0, 9.0, 24.0],
    sky1: [14.0, 28.0, 56.0],
    w0: [10.0, 44.0, 78.0],
    w1: [4.0, 20.0, 44.0],
    w2: [1.0, 6.0, 15.0],
    sand0: [30.0, 44.0, 60.0],
    sand1: [10.0, 16.0, 26.0],
    far: [6.0, 24.0, 42.0],
    far2: [9.0, 32.0, 54.0],
    kelp: [14.0, 54.0, 54.0],
    ink: [2.0, 8.0, 18.0],
};

impl Pal {
    /// `nt` of the way from day to night.
    pub fn at(nt: f32) -> Pal {
        let m = |a: Rgb, b: Rgb| mix(a, b, nt);
        Pal {
            sky0: m(DAY.sky0, NIGHT.sky0),
            sky1: m(DAY.sky1, NIGHT.sky1),
            w0: m(DAY.w0, NIGHT.w0),
            w1: m(DAY.w1, NIGHT.w1),
            w2: m(DAY.w2, NIGHT.w2),
            sand0: m(DAY.sand0, NIGHT.sand0),
            sand1: m(DAY.sand1, NIGHT.sand1),
            far: m(DAY.far, NIGHT.far),
            far2: m(DAY.far2, NIGHT.far2),
            kelp: m(DAY.kelp, NIGHT.kelp),
            ink: m(DAY.ink, NIGHT.ink),
        }
    }
}
