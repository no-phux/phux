//! Predictive local echo: Mosh-class latency hiding.
//!
//! The client paints a guess of what the server will echo, decorated with
//! an underline (dim would collide with apps that paint faint text), and
//! reconciles it cell by cell when authoritative output arrives.
//!
//! - [`PredictionState`] queues guesses and the cursor estimate that
//!   anchors them. Predicted: single grapheme clusters of width 1 or 2
//!   without Ctrl/Alt/Super, Backspace and Ctrl-U bounded by the learned
//!   prompt boundary (the column where typing began on this row), Enter
//!   past column 0, and arrows over a known glyph. Everything else is
//!   sent upstream without a local echo.
//! - [`reconcile_terminal_output_per_cell`] confirms, keeps, or drops the
//!   queue (a contradiction drops the whole suffix).
//! - [`Overlay`] writes the displayable guesses as VT.
//!
//! On the alternate screen display is confirmation-gated (ADR-0090):
//! nothing shows until a non-blank insert is confirmed, so apps that never
//! echo (htop, less, vim normal mode) never show a ghost. Mode-changing
//! input kills that evidence; Enter suspends the burst. Repeated
//! contradictions or an overdue front guess hide the overlay on either
//! screen. The feature is off by default ([`PredictiveConfig`]).

mod overlay;
mod reconcile;
mod state;

pub use overlay::Overlay;
pub use reconcile::{
    ReconcileStats, reconcile_terminal_output_per_cell, reconcile_terminal_output_per_cell_at,
};
pub use state::{Prediction, PredictionKind, PredictionOutcome, PredictionState, PredictiveConfig};
