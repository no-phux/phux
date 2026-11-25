//! How a session stands.

/// Where the session is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No connection has been opened yet.
    Idle,
    /// Dialing, or waiting out the reconnect ladder.
    Connecting,
    /// `HELLO_OK` accepted; no attach has completed on this connection.
    Negotiated,
    /// The attach barrier released; frames are flowing.
    Attached,
    /// The consumer ended the session, or the server detached it at the
    /// consumer's request. Terminal.
    Closed,
    /// A refusal no retry can satisfy, or the initial ladder ran out.
    /// Terminal; the message is in `last_error`.
    Failed,
}

impl Status {
    /// Whether no further transition can happen.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }
}
