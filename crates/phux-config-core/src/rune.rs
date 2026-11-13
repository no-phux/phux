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
    /// List the session used last first, instead of in the arranged order.
    pub recent_first: Option<bool>,
    /// Global chord that shows and hides the quick terminal, such as
    /// `"ctrl+backquote"`; unset or `"none"` leaves it off.
    pub quick_terminal: Option<String>,
    /// Socket of the SSH agent `ssh://` hosts use, such as
    /// `"~/.1password/agent.sock"`: an app opened from the Dock does not
    /// see the shell's `SSH_AUTH_SOCK`.
    pub ssh_agent: Option<String>,
    /// `[rune.keys]`: a chord such as `"cmd+shift+d"` and the command it
    /// runs, or `"none"` to take a built-in chord away.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub keys: std::collections::BTreeMap<String, String>,
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
        let keyed = "[rune.keys]\n\"cmd+d\" = \"none\"\n";
        let parsed = crate::loader::parse_client(keyed, std::path::Path::new("config.toml"));
        assert_eq!(parsed.expect("parses").rune.keys["cmd+d"], "none");
    }
}
