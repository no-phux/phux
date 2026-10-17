//! Where a display preference's value came from, and when an edit applies.
//!
//! Display preferences are saved with the window layout. A Ghostty config
//! seeds them on first launch and on **Use Ghostty look**, so a saved value
//! that still equals what the config sets is attributed to the config line.

use super::display::{DisplayPrefs, default_display};
use super::ghostty::{GhosttyConfig, Origin, ghostty_prefs};

/// The source of one effective display preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// The shipped default.
    Default,
    /// The Ghostty config line that sets this value.
    Ghostty(Origin),
    /// Changed in this app and saved with the desktop layout.
    Desktop,
}

impl Provenance {
    /// Short label for Settings.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Default => "Default".to_owned(),
            Self::Ghostty(origin) => format!("From Ghostty {}", origin.label()),
            Self::Desktop => "Saved desktop preference".to_owned(),
        }
    }

    /// When an edit to the source reaches this display preference.
    #[must_use]
    pub fn applies(&self) -> Applies {
        match self {
            Self::Ghostty(_) => Applies::OnImport,
            Self::Default | Self::Desktop => Applies::Live,
        }
    }
}

/// When an edit takes effect. Every control in Settings applies live. An edit
/// to a Ghostty file reaches binds and the Ghostty theme on a reload, and
/// reaches seeded display preferences only when the look is applied again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// Immediately, in every open view.
    Live,
    /// After **Reload config** reads the Ghostty files again.
    OnReload,
    /// After **Use Ghostty look** copies the config into preferences again.
    OnImport,
}

impl Applies {
    /// Short label for Settings; empty for [`Applies::Live`].
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "",
            Self::OnReload => "Edits to the Ghostty file apply on Reload config.",
            Self::OnImport => "Edits to the Ghostty file apply on Use Ghostty look.",
        }
    }
}

/// The source of the value `field` reads from `prefs`. `ghostty_keys` names
/// the Ghostty keys that can set it, most specific first; the first one the
/// config sets is the origin. Line height, for one, is reset by any of
/// `adjust-cell-height`, `font-size` and `font-family`.
#[must_use]
pub fn display_provenance(
    prefs: &DisplayPrefs,
    ghostty: &GhosttyConfig,
    ghostty_keys: &[&str],
    field: fn(&DisplayPrefs) -> String,
) -> Provenance {
    let current = field(prefs);
    if let Some(origin) = ghostty_keys
        .iter()
        .find_map(|key| ghostty.origins.get(*key))
        && field(&ghostty_prefs(&default_display(), ghostty)) == current
    {
        return Provenance::Ghostty(origin.clone());
    }
    if field(&default_display()) == current {
        Provenance::Default
    } else {
        Provenance::Desktop
    }
}

#[cfg(test)]
mod tests {
    use super::super::display::default_display;
    use super::super::ghostty::parse_ghostty;
    use super::{Applies, Provenance, display_provenance};

    #[test]
    fn provenance_names_default_ghostty_line_or_desktop() {
        let config = parse_ghostty("# comment\nfont-size = 15\n");
        let size = |prefs: &super::DisplayPrefs| prefs.font_size.to_string();
        let mut prefs = default_display();
        assert_eq!(
            display_provenance(&prefs, &config, &["font-size"], size),
            Provenance::Default
        );
        prefs.font_size = 15.0;
        let Provenance::Ghostty(origin) = display_provenance(&prefs, &config, &["font-size"], size)
        else {
            panic!("font size should come from the Ghostty line");
        };
        assert_eq!(origin.line, 2);
        assert_eq!(origin.label(), "line 2");
        prefs.font_size = 18.0;
        assert_eq!(
            display_provenance(&prefs, &config, &["font-size"], size),
            Provenance::Desktop
        );
        let line_height = |prefs: &super::DisplayPrefs| prefs.line_height.to_string();
        prefs.line_height = 1.0;
        let Provenance::Ghostty(origin) = display_provenance(
            &prefs,
            &config,
            &["adjust-cell-height", "font-size", "font-family"],
            line_height,
        ) else {
            panic!("an imported font size resets line height");
        };
        assert_eq!(origin.line, 2);
        assert_eq!(
            display_provenance(&prefs, &config, &[], line_height),
            Provenance::Desktop
        );
        assert_eq!(Applies::Live.label(), "");
        assert!(Applies::OnReload.label().contains("Reload"));
        assert_eq!(Provenance::Desktop.applies(), Applies::Live);
    }
}
