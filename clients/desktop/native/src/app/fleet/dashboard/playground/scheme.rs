//! The playground piece's colour schemes: the fox frames ship in gold, and
//! every other scheme maps each pixel's luminance onto a three-stop ramp
//! (shadow, mid, highlight) and keeps alpha, so the metal's shading survives.
//! The same rows and ramp as phux-mobile's `PieceScheme`; a new scheme is
//! one row in each.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scheme {
    Gold,
    Silver,
    Mint,
    Rose,
    Aurora,
    Ember,
    Matrix,
}

const LOW: f32 = 0.30;
const HIGH: f32 = 0.95;
const MID_STOP: f32 = 0.54;

impl Scheme {
    pub const ALL: [Scheme; 7] = [
        Scheme::Gold,
        Scheme::Silver,
        Scheme::Mint,
        Scheme::Rose,
        Scheme::Aurora,
        Scheme::Ember,
        Scheme::Matrix,
    ];

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|scheme| *scheme == self)
            .unwrap_or(0)
    }

    pub fn id(self) -> &'static str {
        match self {
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
            .unwrap_or(Scheme::Gold)
    }

    pub fn next(self) -> Scheme {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// The button's label: what a click switches to.
    pub fn label(self) -> &'static str {
        match self {
            Scheme::Gold => "Gold piece",
            Scheme::Silver => "Silver piece",
            Scheme::Mint => "Mint piece",
            Scheme::Rose => "Rose piece",
            Scheme::Aurora => "Aurora piece",
            Scheme::Ember => "Ember piece",
            Scheme::Matrix => "Matrix piece",
        }
    }

    /// Shadow, mid and highlight as 0xRRGGBB; None keeps the art's gold.
    fn ramp(self) -> Option<[u32; 3]> {
        match self {
            Scheme::Gold => None,
            Scheme::Silver => Some([0x3A3F47, 0xA9B1BC, 0xF4F6F8]),
            Scheme::Mint => Some([0x0B4A3F, 0x3FC9A0, 0xD9FFF1]),
            Scheme::Rose => Some([0x4A1630, 0xE0679A, 0xFFE3EE]),
            Scheme::Aurora => Some([0x1B1F7A, 0x7A5CFF, 0xC9F4FF]),
            Scheme::Ember => Some([0x4A1A0A, 0xF06A2A, 0xFFE0B8]),
            Scheme::Matrix => Some([0x04260F, 0x22D36B, 0xD8FFE6]),
        }
    }

    /// One straight-alpha pixel's colour in this scheme.
    pub fn recolor(self, rgb: [u8; 3]) -> [u8; 3] {
        let Some([shadow, mid, highlight]) = self.ramp() else {
            return rgb;
        };
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gold_keeps_the_art_and_every_ramp_keeps_the_shading_order() {
        assert_eq!(Scheme::Gold.recolor([199, 139, 63]), [199, 139, 63]);
        for scheme in Scheme::ALL {
            let Some([shadow, _, highlight]) = scheme.ramp() else {
                continue;
            };
            assert_eq!(scheme.recolor([20, 20, 20])[0], (shadow >> 16) as u8);
            assert_eq!(
                scheme.recolor([255, 255, 255])[1],
                ((highlight >> 8) & 0xFF) as u8
            );
            let sum = |c: [u8; 3]| c.iter().map(|v| u32::from(*v)).sum::<u32>();
            assert!(sum(scheme.recolor([230, 200, 150])) > sum(scheme.recolor([120, 90, 40])));
        }
    }

    #[test]
    fn ids_round_trip_and_unknown_is_gold() {
        for scheme in Scheme::ALL {
            assert_eq!(Scheme::from_id(scheme.id()), scheme);
        }
        assert_eq!(Scheme::from_id("plaid"), Scheme::Gold);
        assert_eq!(Scheme::Matrix.next(), Scheme::Gold);
    }
}
