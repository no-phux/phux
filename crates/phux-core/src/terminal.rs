//! [`TerminalFacet`] — the Terminal-kind facet of a [`ResourceDescriptor`].
//!
//! [`ResourceDescriptor`]: crate::resource::ResourceDescriptor

use std::path::PathBuf;

/// The data a Terminal-kind resource carries beyond its identity: grid
/// geometry, launch directory, and a human-set title.
///
/// Pure data — no PTY, no grid contents, no async state. The server keeps
/// the libghostty terminal and PTY plumbing in side tables keyed by the
/// owning resource's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFacet {
    /// Current terminal dimensions in cells, `(cols, rows)`.
    pub dims: (u16, u16),
    /// Working directory the terminal was (or will be) launched from.
    pub cwd: PathBuf,
    /// Optional human-set title, distinct from any title the shell may set.
    pub title: Option<String>,
}
