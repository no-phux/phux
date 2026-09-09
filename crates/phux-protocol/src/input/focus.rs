//! Focus input — the `FocusEvent` wire atom.
//!
//! Per [ADR-0024] the wire owns its atoms: `FocusEvent` is phux-defined and
//! libghostty-free. Under the `server` feature it converts to/from libghostty's
//! `focus::Event`.
//!
//! [ADR-0024]: https://github.com/no-phux/phux/blob/main/ADR/0024-wire-owns-input-atoms.md

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_event_variants() {
        assert_ne!(FocusEvent::Gained, FocusEvent::Lost);
    }

    #[test]
    fn focus_event_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<FocusEvent>();
    }
}
