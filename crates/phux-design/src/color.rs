//! 8-bit sRGB colours, as the tokens and the solver use them.

use std::fmt;

pub use crate::tokens::Rgba as Rgba8;

/// An opaque 8-bit sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Light or dark: which way legibility pushes (§2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    /// The `dark`/`light` key the token documents use.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    #[must_use]
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    /// Where ink moves to read better: white on a dark ground.
    #[must_use]
    pub const fn extreme(self) -> Rgb8 {
        match self {
            Self::Dark => Rgb8::WHITE,
            Self::Light => Rgb8::BLACK,
        }
    }

    /// Where a negative layer step moves: away from the ink.
    #[must_use]
    pub const fn away(self) -> Rgb8 {
        match self {
            Self::Dark => Rgb8::BLACK,
            Self::Light => Rgb8::WHITE,
        }
    }
}

impl Rgb8 {
    pub const WHITE: Self = Self::hex(0xFF_FFFF);
    pub const BLACK: Self = Self::hex(0x00_0000);

    /// The colour `0xRRGGBB`.
    #[must_use]
    pub const fn hex(rgb: u32) -> Self {
        let [_, r, g, b] = rgb.to_be_bytes();
        Self { r, g, b }
    }

    /// `0xRRGGBB`.
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        u32::from_be_bytes([0, self.r, self.g, self.b])
    }

    /// This colour at alpha byte `a`.
    #[must_use]
    pub const fn with_alpha_byte(self, a: u8) -> Rgba8 {
        Rgba8 {
            r: self.r,
            g: self.g,
            b: self.b,
            a,
        }
    }

    /// Opaque.
    #[must_use]
    pub const fn opaque(self) -> Rgba8 {
        self.with_alpha_byte(255)
    }

    /// `#RRGGBB` (any alpha suffix is ignored).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Rgba8::parse(s).map(Rgba8::rgb)
    }
}

impl Rgba8 {
    /// The colour channels without alpha.
    #[must_use]
    pub const fn rgb(self) -> Rgb8 {
        Rgb8 {
            r: self.r,
            g: self.g,
            b: self.b,
        }
    }

    #[must_use]
    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }

    /// `#RRGGBB` or `#RRGGBBAA`, the tokens' two forms.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let digits = s.strip_prefix('#')?;
        if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let n = u32::from_str_radix(digits, 16).ok()?;
        Some(if digits.len() == 6 {
            Rgb8::hex(n).opaque()
        } else {
            Self::hex(n)
        })
    }

    /// The tokens' string form: `#RRGGBB` when opaque, else `#RRGGBBAA`.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Rgb8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

impl fmt::Display for Rgba8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rgb())?;
        if !self.is_opaque() {
            write!(f, "{:02X}", self.a)?;
        }
        Ok(())
    }
}

impl From<Rgb8> for Rgba8 {
    fn from(c: Rgb8) -> Self {
        c.opaque()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_forms_round_trip() {
        for s in ["#0D1017", "#E8E5DF24", "#00000099", "#FFFFFF"] {
            assert_eq!(Rgba8::parse(s).unwrap().to_hex(), s);
        }
        assert_eq!(Rgba8::parse("#e8e5df").unwrap().to_hex(), "#E8E5DF");
        assert_eq!(Rgba8::parse("#E8E5DFFF").unwrap().to_hex(), "#E8E5DF");
        for bad in ["E8E5DF", "#E8E5D", "#E8E5DFZZ", "#E8E5DF2"] {
            assert_eq!(Rgba8::parse(bad), None, "{bad}");
        }
    }
}
