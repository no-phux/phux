//! The playground piece's colours: the Emblem Fox frames ship as a neutral
//! grey with their shading stretched to the full tonal range, and every
//! scheme maps each pixel's luminance onto a three-stop ramp (shadow, mid,
//! highlight) and keeps alpha, so the shading survives. `Theme`, the default,
//! takes its stops from the window's theme text colours, so the piece wears
//! every theme the app does; the named schemes are fixed ramps. The same rows
//! and ramp as phux-mobile's `PieceScheme`; a new scheme is one row in each.

/// Shadow, mid and highlight as 0xRRGGBB.
pub(crate) type Ramp = [u32; 3];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scheme {
    Theme,
    Gold,
    Silver,
    Mint,
    Rose,
    Aurora,
    Ember,
    Matrix,
}

const LOW: f32 = 0.05;
const HIGH: f32 = 0.95;
const MID_STOP: f32 = 0.5;

impl Scheme {
    pub const ALL: [Scheme; 8] = [
        Scheme::Theme,
        Scheme::Gold,
        Scheme::Silver,
        Scheme::Mint,
        Scheme::Rose,
        Scheme::Aurora,
        Scheme::Ember,
        Scheme::Matrix,
    ];

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|scheme| *scheme == self)
            .unwrap_or(0)
    }

    pub fn id(self) -> &'static str {
        match self {
            Scheme::Theme => "theme",
            Scheme::Gold => "gold",
            Scheme::Silver => "silver",
            Scheme::Mint => "mint",
            Scheme::Rose => "rose",
            Scheme::Aurora => "aurora",
            Scheme::Ember => "ember",
            Scheme::Matrix => "matrix",
        }
    }

    pub fn from_id(id: &str) -> Scheme {
        Self::ALL
            .into_iter()
            .find(|scheme| scheme.id() == id)
            .unwrap_or(Scheme::Theme)
    }

    pub fn next(self) -> Scheme {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// The button's label: what a click switches to.
    pub fn label(self) -> &'static str {
        match self {
            Scheme::Theme => "Theme piece",
            Scheme::Gold => "Gold piece",
            Scheme::Silver => "Silver piece",
            Scheme::Mint => "Mint piece",
            Scheme::Rose => "Rose piece",
            Scheme::Aurora => "Aurora piece",
            Scheme::Ember => "Ember piece",
            Scheme::Matrix => "Matrix piece",
        }
    }

    /// This scheme's ramp; `Theme` is the window theme's text tiers.
    pub fn ramp(self, theme: Ramp) -> Ramp {
        match self {
            Scheme::Theme => theme,
            Scheme::Gold => [0x5A3A12, 0xC78B3F, 0xF7E2B5],
            Scheme::Silver => [0x3A3F47, 0xA9B1BC, 0xF4F6F8],
            Scheme::Mint => [0x0B4A3F, 0x3FC9A0, 0xD9FFF1],
            Scheme::Rose => [0x4A1630, 0xE0679A, 0xFFE3EE],
            Scheme::Aurora => [0x1B1F7A, 0x7A5CFF, 0xC9F4FF],
            Scheme::Ember => [0x4A1A0A, 0xF06A2A, 0xFFE0B8],
            Scheme::Matrix => [0x04260F, 0x22D36B, 0xD8FFE6],
        }
    }
}

fn luminance(rgb: u32) -> f32 {
    let channel = |shift: u32| ((rgb >> shift) & 0xFF) as f32;
    (0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)) / 255.
}

/// A theme's three text tiers, darkest first: a light piece on a dark theme
/// and a dark one on a light theme, as its app icon draws it.
pub(crate) fn theme_ramp(mut tiers: Ramp) -> Ramp {
    tiers.sort_by(|a, b| luminance(*a).total_cmp(&luminance(*b)));
    tiers
}

/// One straight-alpha pixel's colour on `ramp`.
pub(crate) fn recolor(ramp: Ramp, rgb: [u8; 3]) -> [u8; 3] {
    let [shadow, mid, highlight] = ramp;
    let lum =
        (0.2126 * f32::from(rgb[0]) + 0.7152 * f32::from(rgb[1]) + 0.0722 * f32::from(rgb[2]))
            / 255.;
    let along = ((lum - LOW) / (HIGH - LOW)).clamp(0., 1.);
    let (from, to, part) = if along < MID_STOP {
        (shadow, mid, along / MID_STOP)
    } else {
        (mid, highlight, (along - MID_STOP) / (1. - MID_STOP))
    };
    let channel = |shift: u32| {
        let start = ((from >> shift) & 0xFF) as f32;
        let end = ((to >> shift) & 0xFF) as f32;
        (start + (end - start) * part).round() as u8
    };
    [channel(16), channel(8), channel(0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ramp_maps_shadow_to_shadow_and_keeps_the_shading_order() {
        let theme = theme_ramp([0xE7E2DA, 0x8A8F96, 0xB8BCC2]);
        assert_eq!(theme, [0x8A8F96, 0xB8BCC2, 0xE7E2DA], "darkest first");
        for scheme in Scheme::ALL {
            let ramp = scheme.ramp(theme);
            assert_eq!(recolor(ramp, [5, 5, 5])[0], (ramp[0] >> 16) as u8);
            assert_eq!(
                recolor(ramp, [255, 255, 255])[1],
                ((ramp[2] >> 8) & 0xFF) as u8
            );
            let sum = |c: [u8; 3]| c.iter().map(|v| u32::from(*v)).sum::<u32>();
            assert!(sum(recolor(ramp, [200, 200, 200])) > sum(recolor(ramp, [60, 60, 60])));
        }
    }

    #[test]
    fn ids_round_trip_and_unknown_is_theme() {
        for scheme in Scheme::ALL {
            assert_eq!(Scheme::from_id(scheme.id()), scheme);
        }
        assert_eq!(Scheme::from_id("plaid"), Scheme::Theme);
        assert_eq!(Scheme::Matrix.next(), Scheme::Theme);
    }
}
