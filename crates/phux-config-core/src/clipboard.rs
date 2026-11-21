//! `defaults.clipboard-read` and `defaults.clipboard-write`, which every
//! client applies to its own clipboard
//! (`docs/program-sequences.md#clipboard-osc-52`).

use serde::{Deserialize, Serialize};

/// What a client does with an OSC 52 clipboard read from its focused pane.
/// Ghostty's `clipboard-read`.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash, schemars::JsonSchema,
)]
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

/// What a client does with an OSC 52 clipboard write from a pane
/// (`defaults.clipboard-write`, ADR-0158). Ghostty's `clipboard-write`.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardWrite {
    /// Set the host clipboard (default, as Ghostty and tmux).
    #[default]
    Allow,
    /// Ask before setting it.
    Ask,
    /// Drop the write.
    Deny,
}

/// The `[defaults]` keys a client reads; the rest belong to the server and
/// the terminal UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ClientDefaults {
    /// `defaults.clipboard-read`.
    pub clipboard_read: ClipboardRead,
    /// `defaults.clipboard-write`.
    pub clipboard_write: ClipboardWrite,
    /// `defaults.attach-session`: the session a new window opens.
    pub attach_session: Option<String>,
}
