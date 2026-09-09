//! Opaque, type-distinct identifiers for the multiplexer's domain entities.
//!
//! Each ID is a [`slotmap`] key newtype. They are `Copy`, `Eq`, `Hash`, and
//! `Debug`, and the compiler rejects mixing one kind of ID for another — a
//! [`SessionId`] cannot be passed where a [`WindowId`] is expected.
//!
//! IDs are *opaque*: callers should treat them as cookies and never inspect
//! their bits. They are only meaningful relative to the [`Registry`] that
//! issued them.
//!
//! [`Registry`]: crate::registry::Registry

use slotmap::new_key_type;

new_key_type! {
    /// Identifies a session — the top-level container for windows.
    pub struct SessionId;

    /// Identifies a window — a tab-like container of panes within a session.
    pub struct WindowId;

    /// Identifies a resource — the leaf entity the server serves. A
    /// PTY-backed Terminal is one [`ResourceKind`](crate::resource::ResourceKind);
    /// an agent session is another. All kinds share this key space.
    pub struct ResourceId;
}
