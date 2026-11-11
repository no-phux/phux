//! Host colours: one palette and one hash every client shares, so a machine
//! looks the same on the CLI, the desktop and mobile.
//!
//! A host's colour is [`auto_index`] of its display name unless an override
//! names a palette id (`[[remote]] color`, `[host] color` for this machine).
//! The state colours (amber needs you, mint done, red failed) are reserved
//! and never in the palette.

use serde::{Deserialize, Serialize};

/// The palette as `(id, 0xRRGGBB)`.
///
/// Ids are stable: never reorder, only append, since a stored override and
/// every client's hash index into it. Only the first [`AUTO_COUNT`] are hashed to; the rest are override-only.
pub const PALETTE: [(&str, u32); 10] = [
    ("blue", 0x4F_8B_F0),
    ("violet", 0x9A_78_E8),
    ("magenta", 0xD9_62_9F),
    ("sky", 0x3B_AF_DA),
    ("coral", 0xE8_71_5A),
    ("olive", 0xA8_B5_45),
    ("slate", 0x7C_8D_A6),
    ("copper", 0xC9_92_6B),
    // Override-only: deeper and yellower than the mint "done" state colour.
    ("green", 0x3D_AA_5C),
    ("navy", 0x2F_55_C4),
];

/// How many palette entries the hash picks from. Fixed at 8 so appending an
/// override-only colour never changes a host's automatic one.
pub const AUTO_COUNT: usize = 8;

/// The override value that clears an override.
pub const AUTO: &str = "auto";

/// FNV-1a 32-bit over the UTF-8 bytes of `name`, trimmed and lowercased
/// with Unicode simple (one-to-one) case mapping.
#[must_use]
pub fn hash(name: &str) -> u32 {
    // The first char of a full lowercase is the simple mapping; only U+0130
    // expands, and its simple form is a plain `i`.
    let key: String = name
        .trim()
        .chars()
        .filter_map(|c| c.to_lowercase().next())
        .collect();
    key.bytes().fold(2_166_136_261_u32, |h, byte| {
        (h ^ u32::from(byte)).wrapping_mul(16_777_619)
    })
}

/// The palette index a host gets with no override.
#[must_use]
pub fn auto_index(name: &str) -> usize {
    // A u32 always fits a usize on the 32- and 64-bit targets phux builds.
    hash(name) as usize % AUTO_COUNT
}

/// The palette index of `id`, or `None` for anything else (`auto`, an id a
/// newer phux appended, a typo).
#[must_use]
pub fn index_of(id: &str) -> Option<usize> {
    PALETTE.iter().position(|(known, _)| *known == id.trim())
}

/// A host's resolved colour, as `phux host ls --json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostColor {
    /// Palette id.
    pub id: String,
    /// `#RRGGBB`.
    pub hex: String,
    /// Whether an override chose it rather than the hash.
    #[serde(rename = "override")]
    pub overridden: bool,
}

/// Resolve `name`'s colour. An override that is not a palette id (`auto`, or
/// one this build does not know) falls back to the hash, so a config written
/// by a newer phux still loads.
#[must_use]
pub fn resolve(name: &str, override_id: Option<&str>) -> HostColor {
    let chosen = override_id.and_then(index_of);
    let index = chosen.unwrap_or_else(|| auto_index(name));
    let (id, rgb) = PALETTE[index];
    HostColor {
        id: id.to_owned(),
        hex: format!("#{rgb:06X}"),
        overridden: chosen.is_some(),
    }
}

/// `[host]`: settings for this machine as other clients see it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalHostCfg {
    /// Palette id overriding this machine's automatic colour; absent or
    /// `"auto"` keeps the hash of its short host name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_test_vectors() {
        for (name, h, index, id) in [
            ("mini", 2_324_299_930, 2, "magenta"),
            ("macbook", 1_780_877_181, 5, "olive"),
            ("powerhouse-1", 324_790_352, 0, "blue"),
            ("This Mac", 2_921_507_462, 6, "slate"),
        ] {
            assert_eq!(hash(name), h, "{name}");
            assert_eq!(auto_index(name), index, "{name}");
            assert_eq!(resolve(name, None).id, id, "{name}");
        }
        assert_eq!(hash("  MINI "), hash("mini"));
        assert_eq!(hash("\u{130}STANBUL"), hash("istanbul"));
    }

    #[test]
    fn palette_is_stable() {
        let ids: Vec<_> = PALETTE.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            [
                "blue", "violet", "magenta", "sky", "coral", "olive", "slate", "copper", "green",
                "navy"
            ]
        );
        assert_eq!(resolve("x", Some("blue")).hex, "#4F8BF0");
        assert_eq!(resolve("x", Some("copper")).hex, "#C9926B");
        assert_eq!(resolve("x", Some("green")).hex, "#3DAA5C");
        assert_eq!(resolve("x", Some("navy")).hex, "#2F55C4");
    }

    #[test]
    fn override_only_colours_are_never_hashed_to() {
        assert_eq!(AUTO_COUNT, 8);
        for n in 0..2_000 {
            assert!(auto_index(&format!("host-{n}")) < AUTO_COUNT);
        }
    }

    #[test]
    fn overrides_resolve_and_unknown_ones_fall_back() {
        let picked = resolve("mini", Some("violet"));
        assert_eq!((picked.id.as_str(), picked.overridden), ("violet", true));
        for fallback in [None, Some("auto"), Some("teal"), Some("")] {
            let auto = resolve("mini", fallback);
            assert_eq!((auto.id.as_str(), auto.overridden), ("magenta", false));
        }
    }
}
