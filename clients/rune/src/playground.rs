// style-literals: illustration (the scenes' shared palette; their chrome
// draws from the tokens in `ui::playground`)
//! Deterministic support and agent vocabulary shared by playground scenes.
//! Scene-specific geometry and simulation stay with each scene.

use crate::activity::Mark;

pub type Rgb = [f32; 3];
pub type Pt = [f32; 2];

/// The waiting and done accents every fleet scene marks agents with.
pub const AMBER: Rgb = [255.0, 181.0, 71.0];
pub const MINT: Rgb = [98.0, 230.0, 180.0];

/// The action an agent's current activity represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mood {
    Working,
    Waiting,
    Done,
    Idle,
}

impl Mood {
    /// Finished work is distinct from waiting for input. A background build
    /// still counts as working even when its agent is idle.
    pub fn of(mark: Mark) -> Mood {
        match mark {
            Mark::Done | Mark::Succeeded => Mood::Done,
            _ if mark.needs_you() => Mood::Waiting,
            _ if mark.live() || mark == Mark::Background => Mood::Working,
            _ => Mood::Idle,
        }
    }

    /// The status ladder state the mood draws as in chrome: its mark,
    /// word and tone (§1.3).
    pub const fn state(self) -> phux_design::State {
        match self {
            Mood::Working => phux_design::State::Working,
            Mood::Waiting => phux_design::State::NeedsYou,
            Mood::Done => phux_design::State::Done,
            Mood::Idle => phux_design::State::Idle,
        }
    }

    /// The scene's own hue for the mood: illustration, not chrome.
    pub fn color(self) -> Rgb {
        match self {
            Mood::Working => [89.0, 212.0, 255.0],
            Mood::Waiting => [255.0, 181.0, 71.0],
            Mood::Done => [98.0, 230.0, 180.0],
            Mood::Idle => [159.0, 176.0, 189.0],
        }
    }

    /// The vocabulary's word for the mood.
    pub fn word(self) -> &'static str {
        self.state().word().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Other,
}

/// A harness's colours, not the geometry any one scene gives it.
#[derive(Debug, PartialEq)]
pub struct Species {
    pub harness: Harness,
    pub body: Rgb,
    pub belly: Rgb,
    pub fin: Rgb,
}

pub const CLAUDE: Species = Species {
    harness: Harness::Claude,
    body: [255.0, 118.0, 58.0],
    belly: [255.0, 216.0, 176.0],
    fin: [255.0, 148.0, 84.0],
};
pub const CODEX: Species = Species {
    harness: Harness::Codex,
    body: [44.0, 102.0, 232.0],
    belly: [150.0, 202.0, 255.0],
    fin: [255.0, 212.0, 64.0],
};
pub const OPENCODE: Species = Species {
    harness: Harness::OpenCode,
    body: [242.0, 196.0, 50.0],
    belly: [255.0, 244.0, 186.0],
    fin: [248.0, 172.0, 36.0],
};
pub const GEMINI: Species = Species {
    harness: Harness::Gemini,
    body: [150.0, 98.0, 242.0],
    belly: [222.0, 204.0, 255.0],
    fin: [104.0, 222.0, 255.0],
};
pub const OTHER: Species = Species {
    harness: Harness::Other,
    body: [112.0, 190.0, 186.0],
    belly: [222.0, 246.0, 240.0],
    fin: [86.0, 170.0, 204.0],
};

/// Declared kind slug first, then the agent name, then the fallback colours.
pub fn species(kind: Option<&str>, name: &str) -> &'static Species {
    let of = |s: &str| {
        let s = s.to_ascii_lowercase();
        [
            ("claude", &CLAUDE),
            ("codex", &CODEX),
            ("opencode", &OPENCODE),
            ("gemini", &GEMINI),
        ]
        .into_iter()
        .find(|(slug, _)| s.starts_with(slug))
        .map(|(_, species)| species)
    };
    kind.and_then(of).or_else(|| of(name)).unwrap_or(&OTHER)
}

/// Mulberry32: identical seeds produce identical scenes.
#[derive(Debug, Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut x = self.0;
        x = (x ^ (x >> 15)).wrapping_mul(x | 1);
        x ^= x.wrapping_add((x ^ (x >> 7)).wrapping_mul(x | 61));
        ((x ^ (x >> 14)) as f64 / 4_294_967_296.0) as f32
    }
}

/// FNV-1a over UTF-16 units, matching the scene prototypes.
pub fn hash(s: &str) -> u32 {
    s.encode_utf16().fold(2_166_136_261u32, |h, unit| {
        (h ^ unit as u32).wrapping_mul(16_777_619)
    })
}

pub fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

/// The share of the way to its target a value closes over `dt` seconds,
/// approaching at `rate` per second (frame-rate independent).
pub fn ease(dt: f32, rate: f32) -> f32 {
    1.0 - (-dt * rate).exp()
}

pub fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    [
        lerp(a[0], b[0], k),
        lerp(a[1], b[1], k),
        lerp(a[2], b[2], k),
    ]
}

#[cfg(test)]
mod tests;
