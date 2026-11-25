//! Snapshot synthesis, input encoding, resize handling, and PTY
//! lifecycle plumbing for [`TerminalActor`].

use super::{
    Bytes, EncodedInputRequest, PtySize, ResourceOutput, ResyncAudience, ResyncReason, SizeReportSize,
    SnapshotBytes, TerminalActor, TerminalInput, debug, trace, warn,
};

mod input;
mod lifecycle;
mod resize;
