//! Themes: Omarchy-schema `colors.toml` files and the catalog that names
//! them (ADR-0157).
//!
//! A theme is a directory holding `colors.toml`: a `mode` plus named
//! colours (`background`, `foreground`, `accent`, `selection`, `muted`, the
//! eight ANSI colours and their bright variants, derived shades). This module
//! parses that file with the same alias and fallback cascade Omarchy's
//! `omarchy-theme-color` applies, so a theme resolves to the same palette
//! here as on the desktop that authored it. Nothing in a theme is executed.
//!
//! Names resolve through [`catalog`]: the managed install directory
//! (`$XDG_DATA_HOME/phux/themes/<name>`) first, then the `[[themes]]` entries
//! of enabled plugins. `[theme]` in `config.toml` selects one with the
//! reserved keys [`NAME_KEY`] or [`FILE_KEY`]; every other key is a renderer
//! slot override layered on top (ADR-0026).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ThemeCfg;
use crate::plugin::PluginManifest;

/// `[theme] name = "<catalog name>"`.
pub const NAME_KEY: &str = "name";
/// `[theme] file = "<path to colors.toml>"`.
pub const FILE_KEY: &str = "file";
/// `[theme]` keys that select a theme rather than override a slot.
pub const RESERVED_KEYS: [&str; 2] = [NAME_KEY, FILE_KEY];
/// The file every theme directory holds.
pub const COLORS_FILE: &str = "colors.toml";

const MAX_COLORS_BYTES: u64 = 64 * 1024;

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// Parse `#rrggbb` or `#rrggbbaa` (alpha dropped). Anything else is
    /// `None`: Omarchy themes also carry `rgb()` lists and gradient specs
    /// for Hyprland, which are not colours phux paints with.
    #[must_use]
    pub fn parse_hex(text: &str) -> Option<Self> {
        let hex = text.trim().strip_prefix('#')?;
        if hex.len() != 6 && hex.len() != 8 {
            return None;
        }
        let channel = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
        Some(Self {
            r: channel(0)?,
            g: channel(2)?,
            b: channel(4)?,
        })
    }

    /// `#rrggbb`, lowercase.
    #[must_use]
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Linear blend `amount` of the way from `self` to `other`, rounded
    /// like Omarchy's `mix_color`.
    #[must_use]
    pub fn mix(self, other: Self, amount: f64) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| {
            let value = f64::from(b).mul_add(amount, f64::from(a) * (1.0 - amount)) + 0.5;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to the channel range first"
            )]
            let channel = value.clamp(0.0, 255.0) as u8;
            channel
        };
        Self {
            r: mix(self.r, other.r),
            g: mix(self.g, other.g),
            b: mix(self.b, other.b),
        }
    }

    /// WCAG 2.1 relative luminance.
    #[must_use]
    pub fn luminance(self) -> f64 {
        let channel = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.0722f64.mul_add(
            channel(self.b),
            0.2126f64.mul_add(channel(self.r), 0.7152 * channel(self.g)),
        )
    }

    /// WCAG 2.1 contrast ratio between two colours (`>= 1.0`).
    #[must_use]
    pub fn contrast_ratio(self, other: Self) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
}

const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };
const WHITE: Rgb = Rgb {
    r: 255,
    g: 255,
    b: 255,
};

/// Whether a theme is drawn for a dark or a light background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Dark background.
    Dark,
    /// Light background.
    Light,
}

/// The palette-0..15 mapping every client applies (ADR-0157 decision 4):
/// Omarchy's Ghostty template, so a theme is the same theme everywhere.
pub const ANSI16_KEYS: [&str; 16] = [
    "background",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "foreground",
    "muted",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_foreground",
];

/// A resolved theme: every semantic key the cascade could fill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemeColors {
    /// Dark or light.
    pub mode: ThemeMode,
    /// Resolved colours by semantic key (`background`, `accent`, ...).
    pub colors: BTreeMap<String, Rgb>,
}

impl ThemeColors {
    /// The colour under `key`, when the theme (or the cascade) defines it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Rgb> {
        self.colors.get(key).copied()
    }

    /// A key the cascade guarantees after a successful load.
    fn required(&self, key: &str) -> Rgb {
        self.get(key).unwrap_or(BLACK)
    }

    /// Terminal default background.
    #[must_use]
    pub fn background(&self) -> Rgb {
        self.required("background")
    }

    /// Terminal default foreground.
    #[must_use]
    pub fn foreground(&self) -> Rgb {
        self.required("foreground")
    }

    /// The one accent hue (falls back to `blue`).
    #[must_use]
    pub fn accent(&self) -> Rgb {
        self.required("accent")
    }

    /// Selection background.
    #[must_use]
    pub fn selection(&self) -> Rgb {
        self.required("selection")
    }

    /// Cursor colour (`bright_foreground`).
    #[must_use]
    pub fn cursor(&self) -> Rgb {
        self.required("cursor")
    }

    /// Palette entries 0..15 in the shared mapping.
    #[must_use]
    pub fn ansi16(&self) -> [Rgb; 16] {
        ANSI16_KEYS.map(|key| self.required(key))
    }
}

/// Why a theme did not load.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ThemeError {
    /// The file could not be read.
    #[error("theme {}: {source}", path.display())]
    Io {
        /// The `colors.toml` path.
        path: PathBuf,
        /// The I/O failure.
        source: std::io::Error,
    },
    /// The file is not TOML.
    #[error("theme {}: {message}", path.display())]
    Parse {
        /// The `colors.toml` path.
        path: PathBuf,
        /// Parse message.
        message: String,
    },
    /// A colour the cascade cannot derive is missing.
    #[error("theme {}: missing `{key}` (and nothing to derive it from)", path.display())]
    Missing {
        /// The `colors.toml` path.
        path: PathBuf,
        /// The semantic key.
        key: &'static str,
    },
    /// No catalog entry has this name.
    #[error("no theme named {0:?}; `phux theme list` shows what is installed")]
    NotFound(String),
    /// A theme name outside `[a-z0-9_][a-z0-9._+-]*`.
    #[error("{0:?} is not a usable theme name (lowercase letters, digits, `.`, `_`, `+`, `-`)")]
    InvalidName(String),
}

/// Load the theme in `dir` (its `colors.toml`, plus an Omarchy `light.mode`
/// marker when the file does not say which mode it is).
///
/// # Errors
///
/// [`ThemeError`] when the file is unreadable, not TOML, or cannot be
/// resolved to a complete palette.
pub fn load_dir(dir: &Path) -> Result<ThemeColors, ThemeError> {
    load_file(&dir.join(COLORS_FILE))
}

/// Load one `colors.toml`.
///
/// # Errors
///
/// [`ThemeError`] when the file is unreadable, not TOML, or cannot be
/// resolved to a complete palette.
pub fn load_file(path: &Path) -> Result<ThemeColors, ThemeError> {
    let io = |source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    };
    let metadata = std::fs::metadata(path).map_err(io)?;
    if metadata.len() > MAX_COLORS_BYTES {
        return Err(ThemeError::Parse {
            path: path.to_path_buf(),
            message: format!("larger than {MAX_COLORS_BYTES} bytes"),
        });
    }
    let text = std::fs::read_to_string(path).map_err(io)?;
    let light_marker = path
        .parent()
        .is_some_and(|dir| dir.join("light.mode").is_file());
    parse(&text, light_marker).map_err(|err| match err {
        ParseError::Toml(message) => ThemeError::Parse {
            path: path.to_path_buf(),
            message,
        },
        ParseError::Missing(key) => ThemeError::Missing {
            path: path.to_path_buf(),
            key,
        },
    })
}

#[derive(Debug)]
enum ParseError {
    Toml(String),
    Missing(&'static str),
}

/// Parse and resolve `colors.toml` text. `light_marker` is Omarchy's
/// `light.mode` file beside it, consulted only when the text names no mode.
fn parse(text: &str, light_marker: bool) -> Result<ThemeColors, ParseError> {
    let raw: BTreeMap<String, toml::Value> =
        toml::from_str(text).map_err(|err| ParseError::Toml(err.message().to_owned()))?;
    let mut strings = BTreeMap::new();
    let mut colors = BTreeMap::new();
    for (key, value) in raw {
        let Some(value) = value.as_str() else {
            continue;
        };
        if let Some(rgb) = Rgb::parse_hex(value) {
            colors.insert(key.clone(), rgb);
        }
        strings.insert(key, value.to_owned());
    }
    resolve(&mut colors)?;
    let mode = match strings
        .get("mode")
        .or_else(|| strings.get("theme_type"))
        .map(|mode| mode.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("light") => ThemeMode::Light,
        Some(_) => ThemeMode::Dark,
        None if light_marker => ThemeMode::Light,
        None => {
            let bg = colors.get("background").copied().unwrap_or(BLACK);
            if u32::from(bg.r) + u32::from(bg.g) + u32::from(bg.b) > 382 {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            }
        }
    };
    Ok(ThemeColors { mode, colors })
}

/// Omarchy's alias and fallback cascade, in its order.
fn resolve(c: &mut BTreeMap<String, Rgb>) -> Result<(), ParseError> {
    fn alias(c: &mut BTreeMap<String, Rgb>, key: &str, from: &str) {
        if !c.contains_key(key)
            && let Some(value) = c.get(from).copied()
        {
            c.insert(key.to_owned(), value);
        }
    }
    fn first(c: &mut BTreeMap<String, Rgb>, key: &str, from: &[&str]) {
        for candidate in from {
            alias(c, key, candidate);
            if c.contains_key(key) {
                return;
            }
        }
    }
    fn derive(c: &mut BTreeMap<String, Rgb>, key: &str, from: &str, towards: Rgb, amount: f64) {
        if !c.contains_key(key)
            && let Some(base) = c.get(from).copied()
        {
            c.insert(key.to_owned(), base.mix(towards, amount));
        }
    }

    // Legacy short names, then the pre-semantic `colorN` palette.
    for (key, legacy) in [
        ("background", "bg"),
        ("dark_background", "dark_bg"),
        ("darker_background", "darker_bg"),
        ("lighter_background", "lighter_bg"),
        ("foreground", "fg"),
        ("dark_foreground", "dark_fg"),
        ("light_foreground", "light_fg"),
        ("bright_foreground", "bright_fg"),
    ] {
        alias(c, key, legacy);
    }
    alias(c, "background", "color0");
    alias(c, "foreground", "color7");
    alias(c, "color0", "background");
    alias(c, "color7", "foreground");
    for (key, legacy) in [
        ("red", "color1"),
        ("green", "color2"),
        ("yellow", "color3"),
        ("blue", "color4"),
        ("magenta", "color5"),
        ("cyan", "color6"),
        ("bright_red", "color9"),
        ("bright_green", "color10"),
        ("bright_yellow", "color11"),
        ("bright_blue", "color12"),
        ("bright_magenta", "color13"),
        ("bright_cyan", "color14"),
    ] {
        alias(c, key, legacy);
    }
    alias(c, "magenta", "purple");
    alias(c, "bright_magenta", "bright_purple");

    first(c, "light_foreground", &["color7", "foreground"]);
    first(c, "bright_foreground", &["color15", "foreground"]);
    c.remove("cursor");
    alias(c, "cursor", "bright_foreground");
    first(c, "lighter_background", &["color0", "background"]);
    first(c, "dark_foreground", &["color8", "foreground"]);
    first(c, "muted", &["color8", "dark_foreground"]);
    first(
        c,
        "selection",
        &["selection_background", "color8", "color0", "background"],
    );
    alias(c, "selection_background", "selection");
    alias(c, "selection_foreground", "bright_foreground");
    alias(c, "orange", "yellow");
    derive(c, "brown", "orange", BLACK, 0.5);
    derive(c, "dark_background", "background", BLACK, 0.25);
    derive(c, "darker_background", "background", BLACK, 0.5);
    for base in ["red", "yellow", "green", "cyan", "blue", "magenta"] {
        derive(c, &format!("bright_{base}"), base, WHITE, 0.2);
    }
    alias(c, "purple", "magenta");
    alias(c, "bright_purple", "bright_magenta");
    // phux's one addition: a theme without an accent accents in blue.
    alias(c, "accent", "blue");

    for key in ANSI16_KEYS.into_iter().chain(["accent", "selection"]) {
        if !c.contains_key(key) {
            return Err(ParseError::Missing(key));
        }
    }
    Ok(())
}

/// Whether `name` may name a catalog entry: Omarchy's theme-name charset,
/// which is also safe as a directory name.
#[must_use]
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && chars.all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '+' | '-')
        })
}

/// The catalog name a theme repo URL implies, as Omarchy derives it.
///
/// That is the last path segment, minus `.git`, minus an `omarchy-` /
/// `phux-` prefix and a `-theme` suffix, lowercased. `None` when that is
/// not a usable name.
#[must_use]
pub fn name_from_url(url: &str) -> Option<String> {
    let mut path = url.trim().trim_end_matches('/');
    // scp-style `user@host:path` has no scheme; drop the host.
    if !path.contains("://")
        && let Some((head, tail)) = path.split_once(':')
        && !head.contains('/')
    {
        path = tail;
    }
    let base = path.rsplit('/').next()?;
    let base = base
        .strip_suffix(".git")
        .unwrap_or(base)
        .to_ascii_lowercase();
    let base = base
        .strip_prefix("omarchy-")
        .or_else(|| base.strip_prefix("phux-"))
        .unwrap_or(&base);
    let base = base.strip_suffix("-theme").unwrap_or(base);
    is_valid_name(base).then(|| base.to_owned())
}

/// The managed theme directory: `$XDG_DATA_HOME/phux/themes`, else
/// `~/.local/share/phux/themes`. `None` when neither variable is set.
#[must_use]
pub fn themes_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("phux").join("themes"));
    }
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("phux")
            .join("themes"),
    )
}

/// Where a catalog entry comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum ThemeSource {
    /// `phux theme install` put it in the managed directory.
    Installed,
    /// An enabled plugin's `[[themes]]` entry.
    Plugin {
        /// The plugin id.
        plugin: String,
    },
}

/// One theme the catalog can resolve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemeEntry {
    /// Catalog name.
    pub name: String,
    /// The directory holding `colors.toml`.
    pub dir: PathBuf,
    /// Installed or plugin-provided.
    pub source: ThemeSource,
}

/// Every theme a name can resolve to, sorted by name: the managed directory
/// first (an install shadows a plugin's theme of the same name), then the
/// `[[themes]]` of `plugins`.
#[must_use]
pub fn catalog(plugins: &[PluginManifest]) -> Vec<ThemeEntry> {
    let mut entries: BTreeMap<String, ThemeEntry> = BTreeMap::new();
    for manifest in plugins {
        for theme in &manifest.themes {
            entries.insert(
                theme.name.clone(),
                ThemeEntry {
                    name: theme.name.clone(),
                    dir: theme.path.clone(),
                    source: ThemeSource::Plugin {
                        plugin: manifest.id.clone(),
                    },
                },
            );
        }
    }
    if let Some(dir) = themes_dir()
        && let Ok(read) = std::fs::read_dir(&dir)
    {
        for entry in read.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let path = entry.path();
            if is_valid_name(&name) && path.join(COLORS_FILE).is_file() {
                entries.insert(
                    name.clone(),
                    ThemeEntry {
                        name,
                        dir: path,
                        source: ThemeSource::Installed,
                    },
                );
            }
        }
    }
    entries.into_values().collect()
}

/// The catalog entry named `name`.
///
/// # Errors
///
/// [`ThemeError::InvalidName`] or [`ThemeError::NotFound`].
pub fn find(name: &str, plugins: &[PluginManifest]) -> Result<ThemeEntry, ThemeError> {
    if !is_valid_name(name) {
        return Err(ThemeError::InvalidName(name.to_owned()));
    }
    catalog(plugins)
        .into_iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| ThemeError::NotFound(name.to_owned()))
}

/// What `[theme]` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSelection<'a> {
    /// Neither reserved key is set: the renderer's shipped palette.
    None,
    /// `file = "..."`: this `colors.toml` (wins over `name`).
    File(&'a str),
    /// `name = "..."`: this catalog entry.
    Name(&'a str),
}

impl ThemeCfg {
    /// Which theme the reserved keys select.
    #[must_use]
    pub fn selection(&self) -> ThemeSelection<'_> {
        self.slots.get(FILE_KEY).map_or_else(
            || {
                self.slots
                    .get(NAME_KEY)
                    .map_or(ThemeSelection::None, |name| ThemeSelection::Name(name))
            },
            |file| ThemeSelection::File(file),
        )
    }

    /// The `slot -> color` overrides: every key but the reserved ones.
    pub fn slot_overrides(&self) -> impl Iterator<Item = (&String, &String)> {
        self.slots
            .iter()
            .filter(|(key, _)| !RESERVED_KEYS.contains(&key.as_str()))
    }
}

/// Load the theme `[theme]` selects, if any. A `file` path may start with
/// `~/`, which expands against `$HOME`.
///
/// # Errors
///
/// [`ThemeError`] when the selection names something that does not exist or
/// does not load. [`ThemeSelection::None`] is `Ok(None)`.
pub fn resolve_selected(
    cfg: &ThemeCfg,
    plugins: &[PluginManifest],
) -> Result<Option<ThemeColors>, ThemeError> {
    match cfg.selection() {
        ThemeSelection::None => Ok(None),
        ThemeSelection::File(file) => load_file(&expand_home(file)).map(Some),
        ThemeSelection::Name(name) => load_dir(&find(name, plugins)?.dir).map(Some),
    }
}

/// `~/x` to `$HOME/x`; anything else unchanged.
#[must_use]
pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    const TOKYO_NIGHT: &str = r##"
mode = "dark"

accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"

background = "#1a1b26"
dark_background = "#13141c"
lighter_background = "#24283b"

foreground = "#a9b1d6"
dark_foreground = "#565f89"
bright_foreground = "#c0caf5"

red = "#f7768e"
yellow = "#e0af68"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"

bright_red = "#ff7a93"
bright_blue = "#7da6ff"
"##;

    fn rgb(hex: &str) -> Rgb {
        Rgb::parse_hex(hex).expect("test hex")
    }

    #[test]
    fn a_semantic_theme_resolves_and_derives_what_it_omits() {
        let theme = parse(TOKYO_NIGHT, false).expect("loads");
        assert_eq!(theme.mode, ThemeMode::Dark);
        assert_eq!(theme.background(), rgb("#1a1b26"));
        assert_eq!(theme.accent(), rgb("#7aa2f7"));
        assert_eq!(theme.cursor(), rgb("#c0caf5"));
        // Omitted bright colours blend 20% towards white, like Omarchy.
        assert_eq!(
            theme.get("bright_green"),
            Some(rgb("#9ece6a").mix(WHITE, 0.2))
        );
        // Stated ones are kept verbatim.
        assert_eq!(theme.get("bright_red"), Some(rgb("#ff7a93")));
        assert_eq!(theme.get("orange"), Some(rgb("#e0af68")));
        assert_eq!(
            theme.get("darker_background"),
            Some(rgb("#1a1b26").mix(BLACK, 0.5))
        );
        let ansi = theme.ansi16();
        assert_eq!(ansi[0], theme.background());
        assert_eq!(ansi[7], theme.foreground());
        assert_eq!(ansi[8], rgb("#414868"));
        assert_eq!(ansi[15], rgb("#c0caf5"));
    }

    #[test]
    fn a_legacy_color_n_theme_still_resolves() {
        let legacy = r##"
color0 = "#000000"
color7 = "#cccccc"
color1 = "#ff0000"
color2 = "#00ff00"
color3 = "#ffff00"
color4 = "#0000ff"
color5 = "#ff00ff"
color6 = "#00ffff"
color8 = "#555555"
color15 = "#ffffff"
"##;
        let theme = parse(legacy, false).expect("loads");
        assert_eq!(theme.background(), rgb("#000000"));
        assert_eq!(theme.foreground(), rgb("#cccccc"));
        assert_eq!(theme.get("muted"), Some(rgb("#555555")));
        assert_eq!(theme.selection(), rgb("#555555"));
        assert_eq!(theme.accent(), rgb("#0000ff"));
        assert_eq!(theme.mode, ThemeMode::Dark);
    }

    #[test]
    fn mode_falls_back_to_the_marker_then_luminance() {
        let no_mode = "background = \"#ffffff\"\nforeground = \"#000000\"\nred = \"#f00000\"\n\
                       green = \"#00f000\"\nyellow = \"#f0f000\"\nblue = \"#0000f0\"\n\
                       magenta = \"#f000f0\"\ncyan = \"#00f0f0\"\n";
        assert_eq!(parse(no_mode, false).expect("loads").mode, ThemeMode::Light);
        let dark_bg = no_mode.replace("#ffffff", "#101010");
        assert_eq!(parse(&dark_bg, false).expect("loads").mode, ThemeMode::Dark);
        assert_eq!(parse(&dark_bg, true).expect("loads").mode, ThemeMode::Light);
        let said = format!("mode = \"dark\"\n{no_mode}");
        assert_eq!(parse(&said, true).expect("loads").mode, ThemeMode::Dark);
    }

    #[test]
    fn a_theme_missing_a_base_colour_is_refused() {
        let err = parse(
            "background = \"#000000\"\nforeground = \"#ffffff\"\n",
            false,
        );
        assert!(matches!(err, Err(ParseError::Missing("red"))));
        assert!(matches!(parse("not toml", false), Err(ParseError::Toml(_))));
    }

    #[test]
    fn non_hex_values_are_ignored_not_fatal() {
        let text = format!("{TOKYO_NIGHT}\nborder = \"rgb(1,2,3) 45deg\"\n");
        let theme = parse(&text, false).expect("loads");
        assert_eq!(theme.get("border"), None);
    }

    #[test]
    fn names_derive_from_urls_the_way_omarchy_does() {
        for (url, want) in [
            (
                "https://github.com/x/omarchy-tokyo-night-theme.git",
                Some("tokyo-night"),
            ),
            (
                "git@github.com:x/Omarchy-Rose-Pine-Theme",
                Some("rose-pine"),
            ),
            ("https://example.com/phux-nord-theme/", Some("nord")),
            ("https://example.com/plain", Some("plain")),
            ("git@host:solo.git", Some("solo")),
            ("https://example.com/..", None),
            ("https://example.com/-flag", None),
            ("https://example.com/a';'id", None),
        ] {
            assert_eq!(name_from_url(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn reserved_keys_select_and_the_rest_override() {
        let mut cfg = ThemeCfg::default();
        assert_eq!(cfg.selection(), ThemeSelection::None);
        cfg.slots.insert("name".into(), "nord".into());
        cfg.slots.insert("accent".into(), "#123456".into());
        assert_eq!(cfg.selection(), ThemeSelection::Name("nord"));
        cfg.slots.insert("file".into(), "/x/colors.toml".into());
        assert_eq!(cfg.selection(), ThemeSelection::File("/x/colors.toml"));
        let overrides: Vec<_> = cfg.slot_overrides().map(|(k, _)| k.as_str()).collect();
        assert_eq!(overrides, ["accent"]);
    }

    #[test]
    fn contrast_ratio_matches_wcag_endpoints() {
        assert!((WHITE.contrast_ratio(BLACK) - 21.0).abs() < 0.01);
        assert!((WHITE.contrast_ratio(WHITE) - 1.0).abs() < 1e-9);
        assert_eq!(rgb("#7aa2f7").hex(), "#7aa2f7");
        assert_eq!(Rgb::parse_hex("#7aa2f7ff"), Some(rgb("#7aa2f7")));
        assert_eq!(Rgb::parse_hex("#fff"), None);
    }
}
