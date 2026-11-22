//! Focus input: the libghostty-free `FocusEvent` wire atom (ADR-0024).

/// Host-window focus change reported by a client.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    /// The client window gained focus.
    Gained = 0,
    /// The client window lost focus.
    Lost = 1,
}
