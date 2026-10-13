//! Hex colours from the settings theme, as GPUI paint colours.

use gpui::{Hsla, rgb};

/// Parse `#RRGGBB` or `#RRGGBBAA`.
pub fn from_hex(color: &str) -> Hsla {
    let hex = color.trim().trim_start_matches('#');
    let (body, alpha) = if hex.len() == 8 {
        (
            &hex[..6],
            u8::from_str_radix(&hex[6..], 16).unwrap_or(255) as f32 / 255.,
        )
    } else if hex.len() == 6 {
        (hex, 1.)
    } else {
        return rgb(0).into();
    };
    let value = u32::from_str_radix(body, 16).unwrap_or(0);
    rgb(value).opacity(alpha).into()
}
