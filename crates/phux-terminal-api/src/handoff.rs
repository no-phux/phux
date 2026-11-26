//! What a Terminal hands its app to carry across a graceful upgrade
//! (ADR-0032): the PTY to re-adopt and a replay snapshot, as plain data.

use std::os::fd::OwnedFd;

use tokio::sync::oneshot;

/// Request a Terminal's graceful-upgrade handoff (ADR-0032); read-only.
#[derive(Debug)]
pub struct UpgradeHandleRequest {
    /// Reply channel; dropping the receiver is benign.
    pub reply: oneshot::Sender<ProcessHandoff>,
}

/// One Terminal's upgrade handoff.
///
/// The descriptors to re-adopt plus a replay snapshot. `master_fd`/`child_pid`
/// are `None` without a live process (skipped). The fields name what the
/// upgrade blob persists.
#[derive(Debug)]
pub struct ProcessHandoff {
    /// Owned duplicate of the master fd, taken under the master lock.
    pub master_fd: Option<OwnedFd>,
    /// Child PID on the slave side, re-adopted via `waitpid` after the exec.
    pub child_pid: Option<i32>,
    /// Current width in cells.
    pub cols: u16,
    /// Current height in cells.
    pub rows: u16,
    /// Cell size in pixels, if a client reported one.
    pub cell_px: Option<(u16, u16)>,
    /// The live title the child set, if any. Not the user-set title, which
    /// lives on the registry.
    pub osc_title: Option<String>,
    /// Live cwd, falling back to the last known one.
    pub cwd: Option<String>,
    /// Replayable viewport snapshot.
    pub vt_replay_bytes: Vec<u8>,
    /// Replayable history that precedes the viewport, or empty.
    pub scrollback_bytes: Vec<u8>,
    /// The pane's OSC 7577 token, so a report after the upgrade still verifies.
    /// [`Debug`] prints only that a token is present.
    pub program_resume_token: Option<RedactedToken>,
}

/// A token that debug output never shows. Serialized only inside the upgrade blob.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RedactedToken(String);

impl RedactedToken {
    /// Wrap `token`.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Borrow the token. Callers must not log it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RedactedToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RedactedToken([redacted])")
    }
}
