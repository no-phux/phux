//! Synthesize a `TERMINAL_SNAPSHOT` `vt_replay_bytes` blob from a
//! `libghostty_vt::Terminal`.
//!
//! See [`synthesizer::SnapshotSynthesizer`] for the main entry point.

/// Reference grid model used to cross-check the synthesizer output.
pub mod reference;
pub mod synthesizer;

pub use reference::ConsumerReference;
pub use synthesizer::{
    GRAPHEME_INLINE, SCROLLBACK_ALL, SnapshotBytes, SnapshotSynthesizer, SynthesisError, synthesize,
};
