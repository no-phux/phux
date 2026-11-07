//! The reef map's world: each machine a reef across the panel's width, each
//! agent a fish whose species is its harness and whose swimming is its mood.
//! A seeded, stepped simulation (ported from the reef prototype, the same
//! constants); `reef_paint` draws it. Nothing here touches gpui.

use std::f32::consts::{PI, TAU};

use super::tier::{REEF_FISH, TIERS, tier};
use crate::app::fleet::Agent;
use crate::app::pane_identity::Harness;
use crate::connection::{AgentInfo, AgentState};

mod grow;
mod sim;
mod species;
mod step;
#[cfg(test)]
mod tests;
mod zone;

pub(crate) use self::grow::*;
pub(crate) use self::sim::*;
pub(crate) use self::species::*;
pub(crate) use self::zone::*;

pub(crate) type Rgb = [f32; 3];

pub(crate) fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * k,
        a[1] + (b[1] - a[1]) * k,
        a[2] + (b[2] - a[2]) * k,
    ]
}

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

/// mulberry32, as the prototype seeds every reef and fish.
#[derive(Clone)]
pub(crate) struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut x = self.0;
        x = (x ^ (x >> 15)).wrapping_mul(x | 1);
        x ^= x.wrapping_add((x ^ (x >> 7)).wrapping_mul(x | 61));
        ((x ^ (x >> 14)) as f64 / 4_294_967_296.) as f32
    }
}

/// FNV-1a over the name's UTF-16 units, so a host's reef is the same reef
/// on every launch and every platform.
pub(crate) fn hash(text: &str) -> u32 {
    let mut h: u32 = 2_166_136_261;
    let mut unit = [0u16; 2];
    for ch in text.chars() {
        h ^= u32::from(ch.encode_utf16(&mut unit)[0]);
        h = h.wrapping_mul(16_777_619);
    }
    h
}
