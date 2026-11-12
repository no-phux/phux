//! `defaults.clipboard-read`, which every client applies to its own
//! clipboard (`docs/program-sequences.md#clipboard-osc-52`).

use serde::{Deserialize, Serialize};

/// What a client does with an OSC 52 clipboard read from its focused pane.
/// Ghostty's `clipboard-read`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardRead {
    /// Answer with the client's clipboard.
    Allow,
    /// Ask before answering.
    Ask,
    /// Never answer (default).
    #[default]
    Deny,
}

/// The `[defaults]` keys a client reads; the rest belong to the server and
/// the terminal UI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ClientDefaults {
    /// `defaults.clipboard-read`.
    pub clipboard_read: ClipboardRead,
}
