//! `[keybindings]`: the prefix, the prefix and global tables, key modes,
//! and the action shape every binding, hook, and status slot shares.

#![allow(clippy::derive_partial_eq_without_eq)] // `toml::Value` is not `Eq`

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::keybind::KeyChord;

/// `[keybindings]` table: prefix key, prefix-table, global table, and key
/// modes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct KeybindingsCfg {
    /// Prefix chord (default `C-a`; some emulators swallow `C-Space`).
    pub prefix: String,
    /// Bindings that fire after the prefix.
    #[serde(rename = "prefix-table")]
    pub prefix_table: BTreeMap<String, Action>,
    /// Bindings that fire any time.
    pub global: BTreeMap<String, Action>,
    /// Key modes (`[keybindings.modes.<name>]`): named tables whose bindings
    /// fire without the prefix while `enter-mode` has the mode active.
    pub modes: BTreeMap<String, BTreeMap<String, Action>>,
    /// Per-mode options (`[keybindings.mode-options.<name>]`).
    #[serde(rename = "mode-options")]
    pub mode_options: BTreeMap<String, ModeOptions>,
    /// Show the which-key popup when no chord follows the prefix within
    /// [`Self::which_key_delay_ms`].
    #[serde(rename = "which-key")]
    pub which_key: bool,
    /// Milliseconds after the prefix before the which-key popup shows.
    #[serde(rename = "which-key-delay-ms")]
    pub which_key_delay_ms: u64,
    /// Prefix-table keys a config layer removed with `"none"`, as chord
    /// sequences (any spelling). Recorded when the layers settle, never read
    /// from TOML, so a plugin's manifest `keys` cannot claim a key you
    /// unbound.
    #[serde(skip)]
    pub unbound: BTreeSet<Vec<KeyChord>>,
}

impl Default for KeybindingsCfg {
    fn default() -> Self {
        Self {
            prefix: "C-a".to_owned(),
            prefix_table: BTreeMap::new(),
            global: BTreeMap::new(),
            modes: BTreeMap::new(),
            mode_options: BTreeMap::new(),
            which_key: true,
            which_key_delay_ms: 400,
            unbound: BTreeSet::new(),
        }
    }
}

/// `[keybindings.mode-options.<name>]`: how a key mode treats keys it does
/// not bind.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ModeOptions {
    /// Swallow keys the mode does not bind instead of handling them as they
    /// are outside the mode (prefix and global bindings, then the pane).
    pub exclusive: bool,
}

/// An action attached to a binding, hook, or status slot: a bare name or an
/// inline table (`docs/consumers/tui.md` §4.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Action {
    /// `"detach"`.
    Bare(String),
    /// `{ action = "new-pane", direction = "vertical" }`.
    Parameterized(ParamAction),
}

/// Parameterized action. Per-action argument validation lives in the
/// dispatcher, not here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParamAction {
    /// The action name. Hooks spell it `kind`; both are accepted.
    #[serde(alias = "kind")]
    pub action: String,
    /// Remaining inline-table fields.
    #[serde(flatten)]
    pub args: BTreeMap<String, toml::Value>,
}
