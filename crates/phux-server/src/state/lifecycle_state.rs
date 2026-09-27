//! The server process's own lifetime: its incarnation, open-connection count
//! and idle clock, self-exit arming, viewport stamps, and upgrade context.
//!
//! * Last-session self-exit is armed only after a client interaction, so a
//!   freshly spawned server whose seed pane dies stays alive.
//! * Idle exit (ADR-0063): `live_connections` and `idle_since` are one
//!   clock, written together only by the open/close methods.
//!
//! Policies that read these live in the runtime. Everything is `pub(super)`
//! and sync.

use std::os::fd::RawFd;
use std::path::PathBuf;
use std::time::Instant;

use super::ServerIncarnation;

/// Everything scoped to the server process.
#[derive(Debug)]
pub(super) struct Lifecycle {
    /// Random identity for this in-memory state incarnation.
    server_incarnation: ServerIncarnation,
    /// Whether last-session self-exit is armed (by an attach or a headless
    /// session create).
    has_served_client: bool,
    /// Monotonic viewport-announcement stamp, so cell-pixel resolution can
    /// prefer the newest report. `pub(super)` for a disjoint-field borrow.
    pub(super) viewport_clock: u64,
    /// Upgrade context (ADR-0032): listener fd, socket path, and runtime
    /// flags to re-pass to the re-exec'd image.
    upgrade_ctx: Option<(RawFd, PathBuf, crate::runtime::RuntimeFlags)>,
    /// Open connections on every transport (not attached clients: one-shot
    /// verbs connect without attaching).
    live_connections: u32,
    /// When connections last dropped to zero; `None` while any is open.
    /// Starts at construction, so a never-dialed server is idle from birth.
    idle_since: Option<Instant>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    /// A fresh process lifetime with a random incarnation.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            server_incarnation: ServerIncarnation::random(),
            has_served_client: false,
            viewport_clock: 0,
            upgrade_ctx: None,
            live_connections: 0,
            // Idle from startup.
            idle_since: Some(Instant::now()),
        }
    }

    /// This state's stable, redaction-safe process incarnation.
    #[must_use]
    pub(super) const fn server_incarnation(&self) -> ServerIncarnation {
        self.server_incarnation
    }

    /// Arm tmux-model last-session self-exit.
    pub(super) const fn arm_self_exit(&mut self) {
        self.has_served_client = true;
    }

    /// Whether last-session self-exit has been armed.
    #[must_use]
    pub(super) const fn has_served_client(&self) -> bool {
        self.has_served_client
    }

    /// Record an accepted connection; disarms the idle clock (idle means
    /// unattended, not silent).
    pub(super) fn note_connection_opened(&mut self) {
        self.live_connections = self.live_connections.saturating_add(1);
        self.idle_since = None;
    }

    /// Record a closed connection; re-arms the idle clock at zero. Saturating,
    /// so a bookkeeping bug fails toward exiting rather than immortality.
    pub(super) fn note_connection_closed(&mut self) {
        self.live_connections = self.live_connections.saturating_sub(1);
        if self.live_connections == 0 {
            self.idle_since = Some(Instant::now());
        }
    }

    /// When the server became unattended, or `None` while connected.
    #[must_use]
    pub(super) const fn idle_since(&self) -> Option<Instant> {
        self.idle_since
    }

    /// Record the upgrade context at startup.
    pub(super) fn set_upgrade_context(
        &mut self,
        listener_fd: RawFd,
        socket_path: PathBuf,
        flags: crate::runtime::RuntimeFlags,
    ) {
        self.upgrade_ctx = Some((listener_fd, socket_path, flags));
    }

    /// The upgrade context, once serving has begun.
    pub(super) fn upgrade_context(
        &self,
    ) -> Option<(RawFd, &std::path::Path, crate::runtime::RuntimeFlags)> {
        self.upgrade_ctx
            .as_ref()
            .map(|(fd, path, flags)| (*fd, path.as_path(), flags.clone()))
    }
}
