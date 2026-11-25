//! `[rune]`: the desktop app's settings, for people who keep them in a file.

use serde::{Deserialize, Serialize};

/// `[rune]` table: settings the Rune app otherwise keeps in its own prefs.
///
/// A key set here wins over the app's saved choice and cannot be changed
/// from Settings; an unset key stays the app's to remember.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct RuneCfg {
    /// Theme id, as the theme picker lists them; `""` follows the system.
    pub theme: Option<String>,
    /// Terminal text size in points each window starts at.
    pub font_size: Option<f32>,
    /// Size of the sidebar, tabs, palette and Settings in percent, 80 to
    /// 200; the terminal's text keeps `font-size`. 100 when unset.
    pub interface_size: Option<u16>,
    /// Terminal font family, or families tried in order before the built-in
    /// faces: a list, or one string with commas between names. `""` is the
    /// built-in face.
    #[serde(deserialize_with = "families")]
    #[schemars(with = "Option<Families>")]
    pub font_family: Option<String>,
    /// Post desktop notifications for agents that need attention.
    pub notifications: Option<bool>,
    /// Option sends Meta (Esc-prefixed) instead of composing characters.
    pub option_as_meta: Option<bool>,
    /// Copy a selection to the clipboard as soon as it is made.
    pub copy_on_select: Option<bool>,
    /// The wheel over a full-screen program that doesn't use the mouse sends
    /// arrow keys, unless the program resets DEC 1007. On when unset.
    pub wheel_arrows: Option<bool>,
    /// Ask before closing a terminal or tab with something running.
    pub confirm_close: Option<bool>,
    /// Bold text in ANSI colours 0 to 7 draws in the bright variant, 8 to
    /// 15, like xterm's `boldColors`. Off when unset; true colour never
    /// brightens.
    pub bold_is_bright: Option<bool>,
    /// List the session used last first, instead of in the arranged order.
    pub recent_first: Option<bool>,
    /// Where the terminal tabs go: `"auto"` in the title bar while the
    /// sidebar is folded (the sidebar lists them while it shows), `"top"`
    /// always in the title bar, `"bottom"` under the panes, or `"hidden"`.
    pub tab_bar: Option<TabBar>,
    /// The title bar: `"shown"`, `"hover"` (hidden until the pointer reaches
    /// the window's top edge) or `"hidden"`.
    pub title_bar: Option<TitleBar>,
    /// The display a new window opens on: `"current"` (where the window was
    /// when it closed), `"mouse"` (the one under the pointer) or `"main"`.
    pub new_window_screen: Option<NewWindowScreen>,
    /// Global chord that shows and hides the quick terminal, such as
    /// `"ctrl+backquote"`; unset or `"none"` leaves it off.
    pub quick_terminal: Option<String>,
    /// The quick terminal's height as a fraction of the display's, 0.2 to
    /// 0.9; 0.4 when unset.
    pub quick_terminal_size: Option<f32>,
    /// The display edge the quick terminal drops from: `"top"` or `"bottom"`.
    pub quick_terminal_edge: Option<QuickTerminalEdge>,
    /// Socket of the SSH agent `ssh://` hosts use, such as
    /// `"~/.1password/agent.sock"`: an app opened from the Dock does not
    /// see the shell's `SSH_AUTH_SOCK`.
    pub ssh_agent: Option<String>,
    /// Markdown renderer argv: UTF-8 Markdown on stdin, plain UTF-8 on stdout.
    /// `{width}` expands to columns. Unset uses bundled tmd; `[]` shows source.
    pub markdown_viewer: Option<Vec<String>>,
    /// The phux session whose ACP agent a tap of the right Command key
    /// prompts; `"chief"` when unset, `"none"` turns the tap off.
    pub chief: Option<String>,
    /// `false` drops the built-in chords: only `[rune.keys]` acts, plus the
    /// palette and Settings chords until you bind those commands.
    pub key_defaults: Option<bool>,
    /// `[rune.keys]`: a chord such as `"cmd+shift+d"` and the command it
    /// runs, or `"none"` to take a built-in chord away.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub keys: std::collections::BTreeMap<String, String>,
}

/// `[rune] quick-terminal-edge`: where the quick terminal opens.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum QuickTerminalEdge {
    /// Across the top of the display.
    #[default]
    Top,
    /// Across the bottom of the display.
    Bottom,
}

/// `[rune] tab-bar`: where the terminal tabs go.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum TabBar {
    /// In the title bar while the sidebar is folded; while it shows, its
    /// rows are the tabs.
    #[default]
    Auto,
    /// In the title bar.
    Top,
    /// Under the panes.
    Bottom,
    /// Nowhere; the palette and shortcuts still switch tabs.
    Hidden,
}

/// `[rune] title-bar`: whether the band over the panes shows.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum TitleBar {
    /// Always.
    #[default]
    Shown,
    /// While the pointer is at the window's top edge.
    Hover,
    /// Never; a thin strip along the top still moves the window.
    Hidden,
}

/// `[rune] new-window-screen`: the display a new window opens on.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum NewWindowScreen {
    /// Where the window was when it closed, on a display still connected.
    #[default]
    Current,
    /// The display under the pointer.
    Mouse,
    /// The main display.
    Main,
}

/// `font-family` as written: one string or a list.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum Families {
    One(String),
    List(Vec<String>),
}

/// `font-family` as one string or a list, kept as one comma-separated string.
fn families<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Some(match Families::deserialize(d)? {
        Families::One(family) => family,
        Families::List(list) => list.join(", "),
    }))
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
        let quick = "[rune]\nquick-terminal-size = 0.5\nquick-terminal-edge = \"bottom\"\n";
        let parsed = crate::loader::parse_client(quick, std::path::Path::new("config.toml"));
        let rune = parsed.expect("parses").rune;
        assert_eq!(rune.quick_terminal_size, Some(0.5));
        assert_eq!(rune.quick_terminal_edge, Some(QuickTerminalEdge::Bottom));
        let listed = "[rune]\nfont-family = [\"Berkeley Mono\", \"Symbols Nerd Font\"]\n";
        let parsed = crate::loader::parse_client(listed, std::path::Path::new("config.toml"));
        let family = parsed.expect("parses").rune.font_family;
        assert_eq!(family.as_deref(), Some("Berkeley Mono, Symbols Nerd Font"));
        let bars = "[rune]\ntab-bar = \"bottom\"\ntitle-bar = \"hidden\"\n";
        let rune = crate::loader::parse_client(bars, std::path::Path::new("config.toml"));
        let rune = rune.expect("parses").rune;
        assert_eq!(
            (rune.tab_bar, rune.title_bar),
            (Some(TabBar::Bottom), Some(TitleBar::Hidden))
        );
        let auto = "[rune]\ntab-bar = \"auto\"\n";
        let auto = crate::loader::parse_client(auto, std::path::Path::new("config.toml"));
        assert_eq!(auto.expect("parses").rune.tab_bar, Some(TabBar::Auto));
        let sideways = "[rune]\nquick-terminal-edge = \"left\"\n";
        assert!(crate::loader::parse_client(sideways, std::path::Path::new("c.toml")).is_err());
    }
}
