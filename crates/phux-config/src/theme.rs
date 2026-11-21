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

mod parse;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::ThemeCfg;
use crate::plugin::PluginManifest;

pub use parse::{ANSI16_KEYS, Rgb, ThemeColors, ThemeError, ThemeMode, load_dir, load_file};

/// `[theme] name = "<catalog name>"`.
pub const NAME_KEY: &str = "name";
/// `[theme] file = "<path to colors.toml>"`.
pub const FILE_KEY: &str = "file";
/// `[theme]` keys that select a theme rather than override a slot.
pub const RESERVED_KEYS: [&str; 2] = [NAME_KEY, FILE_KEY];
/// The file every theme directory holds.
pub const COLORS_FILE: &str = "colors.toml";

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
    let installed = themes_dir();
    catalog_in(installed.as_deref(), plugins)
}

/// [`catalog`] against `installed` instead of [`themes_dir`]. `None`
/// contributes no installed themes.
fn catalog_in(installed: Option<&Path>, plugins: &[PluginManifest]) -> Vec<ThemeEntry> {
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
    if let Some(dir) = installed
        && let Ok(read) = std::fs::read_dir(dir)
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
    let installed = themes_dir();
    find_in(name, plugins, installed.as_deref())
}

/// [`find`] against `installed` instead of [`themes_dir`].
///
/// # Errors
///
/// [`ThemeError::InvalidName`] or [`ThemeError::NotFound`].
fn find_in(
    name: &str,
    plugins: &[PluginManifest],
    installed: Option<&Path>,
) -> Result<ThemeEntry, ThemeError> {
    if !is_valid_name(name) {
        return Err(ThemeError::InvalidName(name.to_owned()));
    }
    catalog_in(installed, plugins)
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
    let installed = themes_dir();
    resolve_selected_in(cfg, plugins, installed.as_deref())
}

/// [`resolve_selected`] against `installed` instead of [`themes_dir`].
///
/// `None` scans no install directory. Plugin themes still come from
/// `plugins`. A [`ThemeSelection::File`] does not consult `installed`;
/// `file` wins over `name`, same as [`ThemeCfg::selection`].
///
/// # Errors
///
/// As [`resolve_selected`].
pub(crate) fn resolve_selected_in(
    cfg: &ThemeCfg,
    plugins: &[PluginManifest],
    installed: Option<&Path>,
) -> Result<Option<ThemeColors>, ThemeError> {
    match cfg.selection() {
        ThemeSelection::None => Ok(None),
        ThemeSelection::File(file) => load_file(&expand_home(file)).map(Some),
        ThemeSelection::Name(name) => load_dir(&find_in(name, plugins, installed)?.dir).map(Some),
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
}
