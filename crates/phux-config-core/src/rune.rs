//! `[rune]`: the desktop app's settings, for people who keep them in a file.

use serde::{Deserialize, Serialize};

/// `[rune]` table: settings the Rune app otherwise keeps in its own prefs.
///
/// A key set here wins over the app's saved choice and cannot be changed
/// from Settings; an unset key stays the app's to remember.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct RuneCfg {
    /// Theme id, as the theme picker lists them; `""` follows the system.
    pub theme: Option<String>,
    /// Terminal text size in points each window starts at.
    pub font_size: Option<f32>,
    /// Terminal font family; `""` is the built-in face.
    pub font_family: Option<String>,
    /// Post desktop notifications for agents that need attention.
    pub notifications: Option<bool>,
    /// Option sends Meta (Esc-prefixed) instead of composing characters.
    pub option_as_meta: Option<bool>,
    /// Copy a selection to the clipboard as soon as it is made.
    pub copy_on_select: Option<bool>,
    /// Ask before closing a terminal or tab with something running.
    pub confirm_close: Option<bool>,
}

impl RuneCfg {
    /// This table as a `[rune]` block to paste into `config.toml`.
    #[must_use]
    pub fn to_toml(&self) -> String {
        format!("[rune]\n{}", toml::to_string(self).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_round_trips_and_unset_keys_stay_out() {
        let cfg = RuneCfg {
            theme: Some("nord".into()),
            font_size: Some(14.0),
            copy_on_select: Some(true),
            ..RuneCfg::default()
        };
        let text = cfg.to_toml();
        assert_eq!(
            text,
            "[rune]\ntheme = \"nord\"\nfont-size = 14.0\ncopy-on-select = true\n"
        );
        let parsed = crate::loader::parse_client(&text, std::path::Path::new("config.toml"));
        assert_eq!(parsed.expect("parses").rune, cfg);
    }
}
