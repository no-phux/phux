//! Agent-detection manifests (ADR-0046).
//!
//! Region extraction, TOML rule compilation, and the offline explanation the
//! CLI prints for `phux agent explain`. The daemon's level-triggered detector
//! (`phux-server::agent_detect`) consumes the same compiled rules; it does
//! not own them. A captured screen can be evaluated with no PTY and no server.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod explain;
pub mod regions;
pub mod rules;

/// A state a manifest rule can assert.
///
/// The wire vocabulary's `unknown` is not representable here: "we do not
/// know" is expressed by publishing nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedState {
    /// Available, not actively working.
    Idle,
    /// Actively doing work.
    Working,
    /// Waiting on a human.
    Blocked,
    /// Finished its task.
    Done,
}

impl DetectedState {
    /// The kebab-case wire word (`docs/spec/L3.md` §3.7).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }
}
