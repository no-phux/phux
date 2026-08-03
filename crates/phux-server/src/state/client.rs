use phux_core::ids::{SessionId, TerminalId};
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::TerminalId as WireTerminalId;
use thiserror::Error;
use tokio::sync::mpsc;

use super::input_log::Outbound;
use crate::terminal_actor::TerminalHandle;

/// Server-assigned identifier for an attached client.
///
/// Distinct from [`phux_protocol::ids::ClientId`] (which is the wire-level
/// identity carried in protocol messages): this one is allocated by the
/// server, monotonic from `1`, and used purely for routing inside
/// [`super::ServerState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

/// An attached client: routing identity plus outbound mailbox.
#[derive(Debug)]
pub struct AttachedClient {
    /// Server-assigned client id.
    pub id: ClientId,
    /// The session this client is observing.
    pub session: SessionId,
    /// Outbound mailbox; the per-client write task drains this and writes to
    /// the socket.
    pub tx: mpsc::Sender<Outbound>,
    /// The client's advertised capabilities (SPEC §6.2). The server MUST
    /// downsample outbound terminal bytes to this set before fanout — see
    /// [`crate::downsample::rewrite_bytes_with_caps`] for the helper the
    /// fanout layer plugs into.
    ///
    /// Populated from the [`phux_protocol::caps::ClientCapabilities`] the
    /// client advertised in HELLO (SPEC §6.1) and forwarded into
    /// [`super::ServerState::attach`]. Test scaffolding that never observed a
    /// HELLO calls [`super::ServerState::attach_default_caps`] which falls back
    /// to [`ClientCapabilities::default`] (most-permissive — never silently
    /// downgrades).
    pub client_caps: ClientCapabilities,
    /// Immutable bootstrap profile selected for this HELLO.
    pub bootstrap_profile: BootstrapProfile,
    /// Immutable payload limits selected for this HELLO.
    pub bootstrap_limits: BootstrapLimits,
    /// This client's current outer viewport (`phux-nk07`).
    ///
    /// Set from the `ATTACH` viewport and updated on every `VIEWPORT_RESIZE`.
    /// The server resolves a shared Terminal's authoritative PTY geometry by
    /// applying the `defaults.window-size` policy across the viewports of
    /// every client subscribed to that Terminal — replacing the old
    /// last-writer-wins resize, where two differently-sized clients thrashed
    /// each other's grid. `None` until the client announces a viewport.
    pub viewport: Option<phux_protocol::wire::frame::ViewportInfo>,
    /// Stamp from [`super::ServerState`]'s viewport clock, taken when
    /// `viewport` was last set. Orders viewport announcements across
    /// clients so cell-pixel resolution
    /// ([`super::ServerState::resolve_terminal_cell_px`]) can prefer the
    /// most recent usable pixel report. `0` until the client announces
    /// a viewport.
    pub viewport_seq: u64,
}

/// One pane target in an ATTACH snapshot pass.
///
/// Bridges the protocol-facing attach flow (`runtime.rs`) to the
/// state-internal registry topology without exposing `Session`/`Window`
/// traversal details outside this module.
#[derive(Debug, Clone)]
pub struct AttachSnapshotPane {
    /// Core pane identifier.
    pub terminal_id: TerminalId,
    /// Cross-task handle for snapshot/input/resize requests.
    pub handle: TerminalHandle,
    /// Stable wire id to use in `TERMINAL_SNAPSHOT` / `TERMINAL_OUTPUT`.
    pub wire_terminal_id: WireTerminalId,
}

/// Errors returned by [`super::ServerState::attach`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AttachError {
    /// No session with that name was found in the registry.
    #[error("unknown session: {0}")]
    UnknownSession(String),
    /// The given [`ClientId`] is already attached.
    #[error("client {0:?} is already attached")]
    AlreadyAttached(ClientId),
    /// The session cannot fit the bounded aggregate attach preflight.
    #[error("session exceeds aggregate attach resource limits")]
    ResourceLimit,
}
