//! The built-in themes: the shared catalog's (`phux_design::catalog`,
//! `crates/phux-design/tokens/themes.json`), the ones it marks as Rune's, in its
//! order. The house terminal (`phux`, `phux-light`) leads.

use std::sync::OnceLock;

use phux_design::{CatalogTheme, Rgb8};

use super::Theme;

fn of(t: &'static CatalogTheme) -> Theme {
    Theme {
        id: t.id,
        name: t.name,
        dark: t.dark,
        ansi: t.ansi.map(Rgb8::to_u32),
        foreground: t.fg.to_u32(),
        background: t.bg.to_u32(),
        cursor: t.cursor.to_u32(),
        // The solver's own fallback, spelled out so every theme has one.
        accent: t.accent.unwrap_or(t.ansi[6]).to_u32(),
        selection: t.selection_bg.map(Rgb8::to_u32),
    }
}

/// Every built-in theme, in picker order.
pub fn themes() -> &'static [Theme] {
    static THEMES: OnceLock<Vec<Theme>> = OnceLock::new();
    THEMES.get_or_init(|| phux_design::catalog().shipped("rune").map(of).collect())
}

/// The built-in theme `id` names, an old id (`midnight`, `catppuccin`)
/// through the catalog's aliases.
pub fn named(id: &str) -> Option<&'static Theme> {
    let id = phux_design::catalog().find(id).map_or(id, |t| t.id);
    themes().iter().find(|t| t.id == id)
}

/// A theme the catalog must hold.
///
/// # Panics
///
/// When the catalog lost `id`, which the tests catch first.
pub(super) fn required(id: &str) -> &'static Theme {
    named(id).unwrap_or_else(|| panic!("the catalog ships {id} to Rune"))
}
