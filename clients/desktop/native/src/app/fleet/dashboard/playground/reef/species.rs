//! Who a fish is: its mood, its harness's species, the water's palette.

use super::*;

/// The galaxy's four moods.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mood {
    Working,
    Waiting,
    Done,
    Idle,
}

impl Mood {
    /// ponytail: the same mapping as `galaxy::Mood::of` in the command
    /// center PR (#2030); switch to that one once it lands.
    pub fn of(info: &AgentInfo) -> Mood {
        match info.state {
            _ if info.urgent() => Mood::Waiting,
            AgentState::Blocked => Mood::Waiting,
            AgentState::Done => Mood::Done,
            AgentState::Working => Mood::Working,
            AgentState::Idle | AgentState::Unknown => Mood::Idle,
        }
    }

    pub fn color(self) -> Rgb {
        match self {
            Mood::Working => [89., 212., 255.],
            Mood::Waiting => [255., 181., 71.],
            Mood::Done => [98., 230., 180.],
            Mood::Idle => [159., 176., 189.],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pattern {
    Bands,
    Mask,
    Lines,
    Spots,
    Plain,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Species {
    pub body: Rgb,
    pub belly: Rgb,
    pub fin: Rgb,
    pub pattern: Pattern,
    pub fat: f32,
    pub tall: f32,
}

/// A harness's fish: clownfish Claude, tang Codex, yellow-tang OpenCode,
/// spotted Gemini, and one teal reef fish for everyone else.
pub(crate) fn species(harness: Harness) -> Species {
    let fish = |body, belly, fin, pattern, fat, tall| Species {
        body,
        belly,
        fin,
        pattern,
        fat,
        tall,
    };
    match harness {
        Harness::Claude => fish(
            [255., 118., 58.],
            [255., 216., 176.],
            [255., 148., 84.],
            Pattern::Bands,
            1.,
            0.9,
        ),
        Harness::Codex => fish(
            [44., 102., 232.],
            [150., 202., 255.],
            [255., 212., 64.],
            Pattern::Mask,
            1.05,
            0.8,
        ),
        Harness::OpenCode => fish(
            [242., 196., 50.],
            [255., 244., 186.],
            [248., 172., 36.],
            Pattern::Lines,
            1.3,
            1.7,
        ),
        Harness::Gemini => fish(
            [150., 98., 242.],
            [222., 204., 255.],
            [104., 222., 255.],
            Pattern::Spots,
            0.88,
            0.7,
        ),
        _ => fish(
            [112., 190., 186.],
            [222., 246., 240.],
            [86., 170., 204.],
            Pattern::Plain,
            0.95,
            0.8,
        ),
    }
}

/// Fish keep their size up to eight on a reef; past that they shrink so a
/// crowded reef still reads, never below 55 percent.
pub(crate) fn crowd(count: usize) -> f32 {
    if count > 8 {
        (8. / count as f32).sqrt().max(0.55)
    } else {
        1.
    }
}

/// The water's colours by day and by night.
#[derive(Clone, Copy)]
pub(crate) struct Pal {
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
    sky0: [120., 196., 232.],
    sky1: [214., 238., 246.],
    w0: [58., 186., 206.],
    w1: [14., 108., 148.],
    w2: [6., 40., 70.],
    sand0: [222., 202., 156.],
    sand1: [128., 112., 86.],
    far: [26., 112., 146.],
    far2: [40., 140., 168.],
    kelp: [72., 132., 70.],
    ink: [4., 22., 36.],
};

const NIGHT: Pal = Pal {
    sky0: [4., 9., 24.],
    sky1: [14., 28., 56.],
    w0: [10., 44., 78.],
    w1: [4., 20., 44.],
    w2: [1., 6., 15.],
    sand0: [30., 44., 60.],
    sand1: [10., 16., 26.],
    far: [6., 24., 42.],
    far2: [9., 32., 54.],
    kelp: [14., 54., 54.],
    ink: [2., 8., 18.],
};

impl Pal {
    /// Day eased `k` of the way to night.
    pub fn at(k: f32) -> Pal {
        let m = |a, b| mix(a, b, k);
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

pub(crate) const CORALS: [Rgb; 6] = [
    [255., 108., 150.],
    [186., 118., 255.],
    [255., 146., 78.],
    [110., 222., 206.],
    [255., 202., 88.],
    [255., 92., 110.],
];
