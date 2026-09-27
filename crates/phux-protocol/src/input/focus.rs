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

#[cfg(feature = "server")]
impl From<FocusEvent> for libghostty_vt::focus::Event {
    fn from(e: FocusEvent) -> Self {
        match e {
            FocusEvent::Gained => Self::Gained,
            FocusEvent::Lost => Self::Lost,
        }
    }
}

#[cfg(feature = "server")]
impl From<libghostty_vt::focus::Event> for FocusEvent {
    fn from(e: libghostty_vt::focus::Event) -> Self {
        match e {
            libghostty_vt::focus::Event::Gained => Self::Gained,
            libghostty_vt::focus::Event::Lost => Self::Lost,
        }
    }
}
