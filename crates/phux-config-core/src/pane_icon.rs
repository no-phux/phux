//! Pane icons: a glyph drawn before a pane's label.
//!
//! A program in the pane can set one (`OSC 1337 ; SetUserVar=phux_icon`,
//! which the server publishes as the `icon` of the pane's
//! `phux.pane-occupant/v1` record). Else the first `[[icons]]` entry in
//! `config.toml` that matches the foreground program or the working folder
//! names one. With neither, the pane has no icon.
//!
//! An icon is text, an emoji or a Nerd Font symbol, so the terminal UI and
//! the desktop draw the same thing with no shared image set.

use std::sync::{LazyLock, PoisonError, RwLock};

use serde::{Deserialize, Deserializer, Serialize};

/// The longest icon, in bytes: room for an emoji ZWJ sequence.
pub const MAX_ICON_BYTES: usize = 32;

/// Whether `icon` is one: 1 to [`MAX_ICON_BYTES`] bytes with no whitespace,
/// control character, or text-direction override, which could reorder the
/// label it sits in.
#[must_use]
pub fn is_icon(icon: &str) -> bool {
    let bidi = |c: char| matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    !icon.is_empty()
        && icon.len() <= MAX_ICON_BYTES
        && !icon
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || bidi(c))
}

/// One `[[icons]]` entry. Every matcher it names must match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IconConfigEntry {
    /// The glyph.
    #[serde(deserialize_with = "icon")]
    pub icon: String,
    /// The foreground program's name, matched exactly (`cargo`, `htop`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// The working folder: `*` matches any run of characters, including `/`,
    /// and a leading `~` is the home folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

fn icon<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let icon = String::deserialize(de)?;
    if is_icon(&icon) {
        return Ok(icon);
    }
    Err(serde::de::Error::custom(format!(
        "{icon:?} is not an icon; use 1 to {MAX_ICON_BYTES} bytes with no spaces"
    )))
}

/// The icon of the first entry whose matchers all hold. An entry with no
/// matcher matches nothing.
#[must_use]
pub fn icon_of<'a>(
    entries: &'a [IconConfigEntry],
    foreground: Option<&str>,
    cwd: Option<&str>,
    home: &str,
) -> Option<&'a str> {
    let holds = |entry: &IconConfigEntry| {
        let process = entry.process.as_deref().map(|p| Some(p) == foreground);
        let folder = entry.cwd.as_deref().map(|pattern| {
            let pattern = pattern
                .strip_prefix('~')
                .map_or_else(|| pattern.to_owned(), |rest| format!("{home}{rest}"));
            cwd.is_some_and(|cwd| glob(&pattern, cwd.trim_end_matches('/')))
        });
        (process.is_some() || folder.is_some()) && process.unwrap_or(true) && folder.unwrap_or(true)
    };
    entries
        .iter()
        .find(|entry| holds(entry))
        .map(|entry| entry.icon.as_str())
}

/// `*` matches any run of characters; everything else matches itself.
fn glob(pattern: &str, text: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

// ponytail: one process-wide table, as Rune keeps its session colours; each
// client process learns it from its own config load and reload.
static KNOWN: LazyLock<RwLock<Vec<IconConfigEntry>>> = LazyLock::new(RwLock::default);

/// Take in the file's `[[icons]]`.
pub fn learn(entries: Vec<IconConfigEntry>) {
    *KNOWN.write().unwrap_or_else(PoisonError::into_inner) = entries;
}

/// The pane's icon: the one its program set, else the learned table's.
#[must_use]
pub fn resolve(
    program: Option<&str>,
    foreground: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    if let Some(icon) = program.filter(|icon| is_icon(icon)) {
        return Some(icon.to_owned());
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let known = KNOWN.read().unwrap_or_else(PoisonError::into_inner);
    icon_of(&known, foreground, cwd, &home).map(str::to_owned)
}

/// `label` with the pane's icon in front, when it has one.
#[must_use]
pub fn with_icon(
    label: String,
    program: Option<&str>,
    foreground: Option<&str>,
    cwd: Option<&str>,
) -> String {
    match resolve(program, foreground, cwd) {
        Some(icon) => format!("{icon} {label}"),
        None => label,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{icon_of, is_icon};
    use crate::loader::parse_client;

    #[test]
    fn the_first_matching_entry_names_the_icon() {
        let input = "[[icons]]\nicon = '🦀'\nprocess = 'cargo'\n\
                     [[icons]]\nicon = '🏢'\ncwd = '~/work/*'\n\
                     [[icons]]\nicon = '📦'\nprocess = 'npm'\ncwd = '/srv/*/web'\n\
                     [[icons]]\nicon = '⭐'\n";
        let icons = parse_client(input, Path::new("config.toml"))
            .expect("parses")
            .icons;
        let of = |fg, cwd| icon_of(&icons, fg, cwd, "/home/me");
        assert_eq!(of(Some("cargo"), Some("/home/me/work/api")), Some("🦀"));
        assert_eq!(of(Some("zsh"), Some("/home/me/work/api/src/")), Some("🏢"));
        assert_eq!(of(Some("zsh"), Some("/home/me/workshop")), None);
        assert_eq!(of(Some("npm"), Some("/srv/a/b/web")), Some("📦"));
        assert_eq!(of(Some("npm"), Some("/srv/a/web/x")), None);
        assert_eq!(of(Some("vim"), None), None);
        assert_eq!(of(None, None), None);
    }

    #[test]
    fn an_icon_is_short_and_has_no_spaces() {
        assert!(is_icon("🦀") && is_icon("\u{f0e7}") && is_icon("👨‍👩‍👧‍👦"));
        for bad in ["", "a b", "\u{1b}[2J", "\u{202e}x", &"x".repeat(33)] {
            assert!(!is_icon(bad), "{bad:?}");
        }
        let err = parse_client("[[icons]]\nicon = 'two words'\n", Path::new("c.toml"))
            .expect_err("refused");
        assert!(err.to_string().contains("is not an icon"), "{err}");
    }
}
