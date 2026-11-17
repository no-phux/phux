//! Per-pane scrollback bounds: the `defaults.history-*` values every
//! terminal engine reads.
//!
//! Here rather than in `phux-config` so the terminal crates can size their
//! scrollback without linking the TUI's schema; `phux-config` re-exports
//! these at their old paths, and `DefaultsCfg::scrollback_limits` builds one
//! from the parsed file.

/// The pair of per-pane scrollback bounds libghostty prunes on together;
/// carried as one value so no plumbing site threads them independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackLimits {
    /// Rows of history (`defaults.history-limit`).
    pub lines: u32,
    /// Bytes of history (`defaults.history-bytes`).
    pub bytes: u32,
}

impl ScrollbackLimits {
    /// Pair an explicit line and byte bound.
    #[must_use]
    pub const fn new(lines: u32, bytes: u32) -> Self {
        Self { lines, bytes }
    }
}

impl Default for ScrollbackLimits {
    fn default() -> Self {
        Self::new(DEFAULT_HISTORY_LINES, DEFAULT_HISTORY_BYTES)
    }
}

/// Shipped `defaults.history-limit`: rows of scrollback per pane.
pub const DEFAULT_HISTORY_LINES: u32 = 50_000;

/// Shipped `defaults.history-bytes`: 10 MiB per pane (ADR-0143). Attach
/// leases history rather than encoding it (ADR-0119), so this prices only
/// the resident memory of a pane that has filled it.
pub const DEFAULT_HISTORY_BYTES: u32 = 10 * 1024 * 1024;
/// Largest accepted `defaults.history-bytes` (64 MiB): a resident-memory
/// bound, held for the life of the session.
pub const MAX_HISTORY_BYTES: u32 = 64 * 1024 * 1024;
