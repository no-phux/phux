//! The server process's own lifetime: who this incarnation is, how many
//! connections it is holding, whether it has earned the right to exit, and
//! the context it needs to hand itself over to a successor image.
//!
//! Six fields that were flat on [`super::ServerState`] live here because
//! they share one lifetime — the *process*'s, not any session's, client's,
//! or pane's. Nothing here is keyed by a domain entity and nothing here is
//! dropped when one dies; every field is either written once at startup or
//! monotonic for as long as the process runs. That is the boundary: state
//! about the server rather than about what the server is serving.
//!
//! Two exit stories run through it and both are timing-sensitive:
//!
//! * **Last-session self-exit** (phux-60s, the tmux model):
//!   `has_served_client` arms it, so a freshly auto-spawned server whose
//!   seed pane dies before any client interaction stays alive instead of
//!   vanishing mid-handshake.
//! * **Idle exit** (ADR-0063, `--exit-after-idle`): `live_connections` and
//!   `idle_since` are two halves of one clock and are only ever written
//!   together, by [`Self::note_connection_opened`] /
//!   [`Self::note_connection_closed`]. Splitting them, or letting a third
//!   party write one without the other, is how a server becomes immortal
//!   (or reaps itself out from under a live connection).
//!
//! `upgrade_ctx` is the graceful-upgrade context (ADR-0032) and is moved
//! here verbatim, tuple and all: `handle_upgrade` reads all three parts
//! together and nothing else reads any of them.
//!
//! # Ownership boundary
//!
//! This type owns the *facts*, not the policies that read them. The idle
//! watchdog's deadline arithmetic lives in `runtime`, the self-exit
//! decision in `runtime::client`'s reap path, and the re-exec in
//! `runtime::upgrade`; each reaches in through the delegating accessors on
//! `ServerState` (see `state::lifecycle`, `state::upgrade_blob`, and
//! `ServerState::server_incarnation` in `state`).
//!
//! One field is `pub(super)`: `viewport_clock`, because
//! `ServerState::set_client_viewport` stamps it while holding a `&mut`
//! borrow of the client table, which only borrow-checks as disjoint field
//! access. The other five are private, so every read and write goes
//! through a method here.
//!
//! Nothing here is `async` and nothing awaits, so the state lock can never
//! be held across a suspension point through this type.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState`, so the crate's public surface is unchanged
//! and the five private fields stay exactly as unreachable from outside
//! `state` as they were as private fields.

use std::os::fd::RawFd;
use std::path::PathBuf;
use std::time::Instant;

use super::ServerIncarnation;

/// Everything scoped to this server *process*: its identity, its
/// connection/idle clocks, its self-exit arming, its viewport stamp
/// source, and its graceful-upgrade context.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct Lifecycle {
    /// Random identity for this in-memory state incarnation.
    server_incarnation: ServerIncarnation,
    /// Whether last-session self-exit has been armed.
    ///
    /// A client attach or successful headless session create arms the
    /// tmux-model self-exit (phux-60s). A freshly auto-spawned server whose
    /// seed pane dies before either interaction stays alive instead of
    /// vanishing mid-handshake — the launching `phux` can repopulate it via
    /// `CreateIfMissing`.
    has_served_client: bool,
    /// Monotonic stamp handed out on every viewport announcement
    /// ([`super::ServerState::set_client_viewport`]). Orders announcements
    /// across clients so
    /// [`super::ServerState::resolve_terminal_cell_px`] can pick the most
    /// recent usable pixel report deterministically.
    ///
    /// `pub(super)` rather than private: its one mutator stamps it while
    /// holding a `&mut` borrow of the client table, so it must be reachable
    /// as a disjoint field path.
    pub(super) viewport_clock: u64,
    /// Graceful-upgrade context (ADR-0032): the listening socket's raw fd,
    /// its path, and the server's effective runtime flags (phux-v45.10),
    /// captured at startup. `handle_upgrade` reads these to build the handoff
    /// blob and to re-pass `--socket` / `--listen` / `--quic` / `--hub` to
    /// the re-exec'd image. `None` until [`Self::set_upgrade_context`] runs
    /// (i.e. before serving).
    upgrade_ctx: Option<(RawFd, PathBuf, crate::runtime::RuntimeFlags)>,
    /// Number of client connections currently open across every transport
    /// (UDS, WebSocket, QUIC, WebTransport). Incremented the instant an
    /// accept succeeds and decremented when that connection's task ends —
    /// see [`Self::note_connection_opened`] /
    /// [`Self::note_connection_closed`].
    ///
    /// This counts *connections*, not attached clients: a one-shot control
    /// verb (`phux ls`, `phux send-keys`, `phux new --json`) connects,
    /// issues a `COMMAND`, and leaves without ever appearing in
    /// [`super::ServerState::attached`]. Only the connection count sees it.
    live_connections: u32,
    /// When the server last had zero open connections, or `None` while at
    /// least one is open.
    ///
    /// Seeded to "now" at construction so a server that has *never* been
    /// connected to is idle from birth — the leak shape that motivated
    /// `--exit-after-idle` is a bootstrapped daemon whose harness died
    /// before it ever dialed in. Read by the runtime's idle-exit watchdog;
    /// nothing else consumes it, and it is inert unless that watchdog was
    /// spawned.
    idle_since: Option<Instant>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    /// Mint a fresh process lifetime: a new incarnation, no connections,
    /// nothing served, no upgrade context.
    ///
    /// Not `Default`-derivable and not `const`: the incarnation comes from
    /// the OS CSPRNG and the idle clock from the monotonic clock.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            server_incarnation: ServerIncarnation::random(),
            has_served_client: false,
            viewport_clock: 0,
            upgrade_ctx: None,
            live_connections: 0,
            // "Idle since startup": a server nobody ever dialed is the
            // leak shape `--exit-after-idle` exists for, so the clock is
            // already running before the first accept.
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

    /// Record that a client connection was accepted (any transport).
    ///
    /// Disarms the idle clock: while a connection is open the server is by
    /// definition not idle, however long that connection sits quiet. "Idle"
    /// here means *unattended*, not *silent* — a human parked in an attached
    /// pane reading a log for an hour must never be reaped.
    pub(super) fn note_connection_opened(&mut self) {
        self.live_connections = self.live_connections.saturating_add(1);
        self.idle_since = None;
    }

    /// Record that a client connection ended (clean EOF, error, or drop).
    ///
    /// Re-arms the idle clock when this was the last one. `saturating_sub`
    /// rather than `-= 1`: a decrement without a matching increment is a
    /// bookkeeping bug, and underflowing to `u32::MAX` would silently make
    /// the server immortal — the exact failure this whole feature exists to
    /// prevent. Saturating instead pins the count at zero, which fails
    /// *toward* exiting and is therefore the safe direction.
    pub(super) fn note_connection_closed(&mut self) {
        self.live_connections = self.live_connections.saturating_sub(1);
        if self.live_connections == 0 {
            self.idle_since = Some(Instant::now());
        }
    }

    /// Instant the server became unattended, or `None` while a client
    /// connection is open. See [`Self::note_connection_opened`].
    #[must_use]
    pub(super) const fn idle_since(&self) -> Option<Instant> {
        self.idle_since
    }

    /// Record the upgrade context — the listening socket's raw fd, path,
    /// and the server's effective runtime flags (phux-v45.10) — at startup,
    /// for `handle_upgrade` to read when building the handoff.
    pub(super) fn set_upgrade_context(
        &mut self,
        listener_fd: RawFd,
        socket_path: PathBuf,
        flags: crate::runtime::RuntimeFlags,
    ) {
        self.upgrade_ctx = Some((listener_fd, socket_path, flags));
    }

    /// The upgrade context `(listener_fd, socket_path, runtime_flags)`, if
    /// serving has begun.
    pub(super) fn upgrade_context(
        &self,
    ) -> Option<(RawFd, &std::path::Path, crate::runtime::RuntimeFlags)> {
        self.upgrade_ctx
            .as_ref()
            .map(|(fd, path, flags)| (*fd, path.as_path(), flags.clone()))
    }
}
