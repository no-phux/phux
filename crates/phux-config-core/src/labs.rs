//! `[developer]` and `[labs]`: developer mode, and the feature flags every
//! part of phux reads from one registry.
//!
//! A lab is a feature that ships turned off (or decided for you) until it is
//! ready. Each one is a row of [`LABS`]; `[labs]` in `config.toml` holds the
//! person's choices by id. The server, the CLI, the terminal UI and Rune read
//! the same file, so one choice reaches the whole system: [`Lab::scope`]
//! says which part acts on it. Clients only show their Labs section while
//! `[developer] mode` is on; that hides the switches, never what they do.
//!
//! The table is a map, not a schema struct, so a lab retired from [`LABS`]
//! stays loadable in an old file: `phux config check` names it instead of
//! the config failing to load.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ExperimentalCfg;

/// Where a lab's behavior lives, and so which process must re-read the file
/// before a change shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LabScope {
    /// The server acts on it: `phux config reload`, or the next start.
    Server,
    /// Each client acts on it for itself, on its next attach.
    Client,
    /// The server and the clients both act on it.
    System,
}

impl LabScope {
    /// The word a settings page or `phux config labs` shows.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
            Self::System => "system",
        }
    }
}

/// One feature flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lab {
    /// The key under `[labs]`: kebab-case, stable for the lab's life.
    pub id: &'static str,
    /// The name a settings page shows.
    pub title: &'static str,
    /// One or two sentences: what turning it on does, and what unset means.
    pub detail: &'static str,
    /// Which part of phux acts on it.
    pub scope: LabScope,
    /// What the lab does when `[labs]` leaves it unset. `None` means the
    /// lab decides per use (see its detail).
    pub default: Option<bool>,
}

/// `[labs] predictive-echo`: Mosh-class predictive local echo (ADR-0090).
pub const PREDICTIVE_ECHO: &str = "predictive-echo";

/// Every lab, in display order.
pub const LABS: &[Lab] = &[Lab {
    id: PREDICTIVE_ECHO,
    title: "Predictive echo",
    detail: "Draws your typing before the server echoes it, so a slow link feels \
             local. Unset turns it on over a remote dial only; true or false applies \
             everywhere.",
    scope: LabScope::Client,
    default: None,
}];

/// The lab registered under `id`, if any.
#[must_use]
pub fn lab(id: &str) -> Option<&'static Lab> {
    LABS.iter().find(|lab| lab.id == id)
}

/// `[developer]`: settings for people working on phux or on top of it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DeveloperCfg {
    /// Developer mode: every client shows its Labs section. Off hides the
    /// switches; labs already set keep their effect.
    pub mode: bool,
}

/// `[labs]`: lab id to on or off. An absent id is the lab's default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(transparent)]
pub struct LabsCfg {
    /// Lab id (a [`LABS`] row) to the person's choice.
    pub flags: BTreeMap<String, bool>,
}

impl LabsCfg {
    /// The choice `[labs]` makes for `id`, or `None` when it leaves it unset.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<bool> {
        self.flags.get(id).copied()
    }

    /// Whether `id` is on: the file's choice, else the lab's default, else
    /// off (an unknown id, or a lab that decides per use).
    #[must_use]
    pub fn enabled(&self, id: &str) -> bool {
        self.get(id)
            .or_else(|| lab(id).and_then(|lab| lab.default))
            .unwrap_or(false)
    }

    /// Ids set in the file that [`LABS`] does not register.
    pub fn unknown(&self) -> impl Iterator<Item = &str> {
        self.flags
            .keys()
            .map(String::as_str)
            .filter(|id| lab(id).is_none())
    }

    /// The explicit predictive-echo choice: `[labs]` first, then the
    /// `[experimental] predictive-echo` key it replaced.
    #[must_use]
    pub fn predictive_echo(&self, legacy: &ExperimentalCfg) -> Option<bool> {
        self.get(PREDICTIVE_ECHO).or(legacy.predictive_echo)
    }

    /// Resolve predictive echo for one attach: an explicit choice wins, else
    /// on only when the dial crosses a network. Local echo is fast enough that
    /// prediction could only cost its two undetectable flicker cases (vi mode
    /// at a readline prompt, no-echo password prompts).
    #[must_use]
    pub fn predictive_echo_for(&self, legacy: &ExperimentalCfg, remote_dial: bool) -> bool {
        self.predictive_echo(legacy).unwrap_or(remote_dial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_kebab_case() {
        let mut seen = std::collections::BTreeSet::new();
        for lab in LABS {
            assert!(seen.insert(lab.id), "duplicate lab {}", lab.id);
            assert!(
                lab.id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{} is not kebab-case",
                lab.id
            );
            assert!(lab.detail.ends_with('.'), "{}", lab.id);
        }
    }

    #[test]
    fn labs_wins_over_the_legacy_key_and_the_dial_decides_unset() {
        let legacy = ExperimentalCfg {
            predictive_echo: Some(true),
        };
        let mut labs = LabsCfg::default();
        assert!(labs.predictive_echo_for(&legacy, false));
        assert!(!labs.predictive_echo_for(&ExperimentalCfg::default(), false));
        assert!(labs.predictive_echo_for(&ExperimentalCfg::default(), true));
        labs.flags.insert(PREDICTIVE_ECHO.to_owned(), false);
        assert!(!labs.predictive_echo_for(&legacy, true));
    }

    #[test]
    fn unknown_ids_are_reported_and_off() {
        let labs: LabsCfg = toml::from_str("retired-lab = true\npredictive-echo = false\n")
            .expect("a retired lab still loads");
        assert_eq!(labs.unknown().collect::<Vec<_>>(), ["retired-lab"]);
        assert!(!labs.enabled(PREDICTIVE_ECHO));
    }
}
