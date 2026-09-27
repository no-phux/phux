//! [`Session`] — the top-level container that owns windows.

use std::time::SystemTime;

use crate::ids::{SessionId, WindowId};

/// A session: a named collection of windows, the unit of attach/detach
/// (ADR-0003).
#[derive(Debug, Clone)]
pub struct Session {
    /// The stable identifier issued by the registry.
    pub id: SessionId,
    /// Human-readable session name; the address clients use to attach.
    pub name: String,
    /// Windows owned by this session, in insertion order.
    pub windows: Vec<WindowId>,
    /// The currently focused window, if any.
    pub active: Option<WindowId>,
    /// When this session was created.
    pub created_at: SystemTime,
    /// Whether this session survives its last window (ADR-0105). Read by the
    /// server's reap cascade, never by the registry.
    pub keep_empty: bool,
}
