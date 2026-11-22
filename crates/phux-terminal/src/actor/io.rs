//! Snapshot synthesis, input encoding, resize handling, and PTY
//! lifecycle plumbing for [`TerminalActor`].

use super::{
    Bytes, EncodedInputRequest, InputEncoderSnapshot, PANE_KILL_POLL, PANE_KILL_REAP_BUDGET,
    PasteOutcome, PtyOwned, PtySize, ResourceOutput, ResyncAudience, ResyncReason, SizeReportSize,
    SnapshotBytes, TerminalActor, TerminalInput, WriteCompletion, debug, error, exit_outcome, mpsc,
    trace, warn,
};

mod input;
mod lifecycle;
mod resize;
