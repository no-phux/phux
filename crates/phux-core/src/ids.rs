//! Opaque, type-distinct [`slotmap`] keys, meaningful only relative to the
//! registry that issued them.

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
