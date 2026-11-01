//! What crosses a graceful upgrade (ADR-0032): plain data an engine hands
//! its host to carry across the exec.

use std::os::fd::OwnedFd;

use tokio::sync::oneshot;

/// Request a resource's graceful-upgrade handoff (ADR-0032); read-only.
#[derive(Debug)]
pub struct UpgradeHandleRequest {
    /// Reply channel; dropping the receiver is benign.
    pub reply: oneshot::Sender<ProcessHandoff>,
}

/// One process-backed resource's upgrade handoff.
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
}
