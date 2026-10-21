//! How a fed frame ended the connection.

/// How a fed frame ended the connection, when it did.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ControlError {
    /// The peer broke the protocol or the kernel could not apply the
    /// frame; the connection should be dropped and redialed for fresh
    /// snapshots.
    #[error("{0}")]
    Protocol(String),
    /// A refusal no retry can satisfy; the session is over.
    #[error("{0}")]
    Refused(String),
    /// A retired, mismatched, or duplicate generation: the C ABI
    /// reports `InvalidState` and the session stays attached.
    #[error("{0}")]
    InvalidState(String),
    /// A replica generation was invalidated; reconnect for fresh
    /// snapshots.
    #[error("a replica needs a fresh bootstrap")]
    Resync,
    /// The server detached the session at this client's request; do not
    /// reconnect.
    #[error("the session was detached at the client's request")]
    Closed,
}

impl ControlError {
    const fn precedence(&self) -> u8 {
        match self {
            Self::InvalidState(_) => 0,
            Self::Protocol(_) => 1,
            Self::Resync => 2,
            Self::Refused(_) | Self::Closed => 3,
        }
    }

    pub(super) fn prefer(current: Option<Self>, candidate: Self) -> Self {
        match current {
            Some(error) if error.precedence() >= candidate.precedence() => error,
            _ => candidate,
        }
    }
}

#[cfg(test)]
mod control_error_tests {
    use super::ControlError;

    #[test]
    fn fatal_batch_errors_outrank_nonfatal_stale_generation_errors() {
        let stale = ControlError::InvalidState("stale".to_owned());
        let protocol = ControlError::Protocol("gap".to_owned());
        let resync = ControlError::Resync;

        let selected = ControlError::prefer(Some(stale), protocol);
        assert!(matches!(selected, ControlError::Protocol(_)));
        let selected = ControlError::prefer(Some(selected), resync);
        assert!(matches!(selected, ControlError::Resync));
    }
}
