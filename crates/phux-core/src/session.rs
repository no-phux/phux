//! [`Session`] — the top-level container that owns windows.

use std::time::SystemTime;

use crate::ids::{SessionId, WindowId};

/// A session: a named collection of windows with one optionally active.
///
/// Sessions are the unit of attach/detach in the multiplexer. A session
/// outlives any individual client connection; clients attach to a session by
/// name (see ADR-0003).
#[derive(Debug, Clone)]
pub struct Session {
    /// The stable identifier issued by the [`Registry`].
    ///
    /// [`Registry`]: crate::registry::Registry
    pub id: SessionId,
    /// Human-readable session name; the address clients use to attach.
    pub name: String,
    /// Windows owned by this session, in insertion order.
    pub windows: Vec<WindowId>,
    /// The currently focused window, if any.
    pub active: Option<WindowId>,
    /// When this session was created.
    pub created_at: SystemTime,
    /// Whether this session survives its last window (ADR-0105).
    ///
    /// A keep-empty session is not reaped when its last window closes; it
    /// stays with zero windows until an explicit kill removes it. The
    /// registry never reads this flag itself: the server's reap cascade does.
    pub keep_empty: bool,
}
